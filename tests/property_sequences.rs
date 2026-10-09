//! Deterministic metamorphic scenarios, not an alternate Store implementation.
//! A record built using a withdrawn source must disappear; an unrelated branch must remain.
use recall_ledger::Store;
use serde_json::{json, Value};

fn run(store: &mut Store, method: &str, params: Value) -> Value {
    store.execute("sequence-tests", method, params).unwrap()
}

fn remember(store: &mut Store, key: &str) -> String {
    run(
        store,
        "remember",
        json!({"body":format!("body-{key}"),"idempotency_key":key}),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn derive(store: &mut Store, key: &str, inputs: &[&str]) -> String {
    let job = run(
        store,
        "prepare",
        json!({
            "inputs":inputs,"processor":"sequence-fixture/v1",
            "idempotency_key":format!("{key}-prepare")
        }),
    );
    run(
        store,
        "publish",
        json!({
            "job_id":job["job_id"],"body":format!("body-{key}"),
            "idempotency_key":format!("{key}-publish")
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn repeated_diamond_graphs_preserve_untouched_branches_under_revise_and_retract() {
    // This fixture has an explicit construction history, so reachability is known
    // without duplicating the production invalidation algorithm in the test.
    for generation in 0..8 {
        for mutation in ["revise", "retract"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("memory.sqlite3");
            let mut store = Store::open(&path).unwrap();
            let changing = remember(&mut store, "changing");
            let stable = remember(&mut store, "stable");
            let independent = derive(&mut store, "independent", &[&stable]);
            let mut descendants = vec![];
            let mut previous = changing.clone();
            for level in 0..=generation {
                let left = derive(&mut store, &format!("left-{level}"), &[&previous]);
                let right = derive(&mut store, &format!("right-{level}"), &[&previous, &stable]);
                let joined = derive(&mut store, &format!("joined-{level}"), &[&left, &right]);
                descendants.extend([left, right, joined.clone()]);
                previous = joined;
            }
            let pending = run(
                &mut store,
                "prepare",
                json!({
                    "inputs":[previous],"processor":"sequence-fixture/v1",
                    "idempotency_key":"pending"
                }),
            );
            let mut arguments = json!({
                "id":changing,"expected_revision":1,"idempotency_key":"mutation"
            });
            if mutation == "revise" {
                arguments["body"] = json!("changed-value");
            }
            run(&mut store, mutation, arguments);
            drop(store);
            let mut store = Store::open(&path).unwrap();
            let response = run(
                &mut store,
                "recall",
                json!({"limit":100,"max_chars":100_000}),
            );
            let visible: Vec<&str> = response["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].as_str().unwrap())
                .collect();
            assert!(visible.contains(&stable.as_str()));
            assert!(visible.contains(&independent.as_str()));
            assert_eq!(visible.contains(&changing.as_str()), mutation == "revise");
            for descendant in descendants {
                assert!(
                    !visible.contains(&descendant.as_str()),
                    "mutation={mutation}, depth={}, leaked descendant={descendant}",
                    generation + 1
                );
            }
            let late = store
                .execute(
                    "sequence-tests",
                    "publish",
                    json!({
                        "job_id":pending["job_id"],"body":"late-result",
                        "idempotency_key":"late"
                    }),
                )
                .expect_err("reopened job resurrected an invalid graph");
            assert_eq!(
                serde_json::to_value(late).unwrap()["code"],
                "stale_dependency"
            );
        }
    }
}

#[test]
fn replaying_committed_requests_after_each_restart_does_not_add_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.sqlite3");
    let mut expected_receipts = vec![];
    for generation in 0..12 {
        let mut store = Store::open(&path).unwrap();
        let request = json!({
            "body":format!("value-{generation}"),
            "idempotency_key":format!("operation-{generation}")
        });
        let receipt = run(&mut store, "remember", request.clone());
        expected_receipts.push((request, receipt));
        for (request, expected) in expected_receipts.iter().rev() {
            assert_eq!(run(&mut store, "remember", request.clone()), *expected);
        }
        let records = run(
            &mut store,
            "recall",
            json!({"limit":100,"max_chars":100_000}),
        );
        assert_eq!(records["items"].as_array().unwrap().len(), generation + 1);
    }
}
