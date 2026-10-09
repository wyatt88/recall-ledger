//! Black-box tests for the public Store contract. No SQL or implementation internals.
use recall_ledger::Store;
use serde_json::{json, Value};
use std::sync::{Arc, Barrier};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const NS: &str = "contract-tests";

fn database() -> (TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    (directory, store)
}

fn call(store: &mut Store, method: &str, params: Value) -> Value {
    store
        .execute(NS, method, params)
        .unwrap_or_else(|error| panic!("{method} failed: {error}"))
}

fn error_code(store: &mut Store, namespace: &str, method: &str, params: Value) -> String {
    let error = store
        .execute(namespace, method, params)
        .expect_err("request unexpectedly succeeded");
    serde_json::to_value(error).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn remember(store: &mut Store, key: &str, body: &str) -> Value {
    call(
        store,
        "remember",
        json!({"body":body,"idempotency_key":key}),
    )
}

fn id(receipt: &Value) -> &str {
    receipt["id"].as_str().expect("receipt must contain id")
}

fn prepare(store: &mut Store, key: &str, ids: &[&str]) -> Value {
    call(
        store,
        "prepare",
        json!({"inputs":ids,"processor":"contract-fixture/v1","idempotency_key":key}),
    )
}

fn publish(store: &mut Store, key: &str, job: &Value, body: &str) -> Value {
    call(
        store,
        "publish",
        json!({"job_id":job["job_id"],"body":body,"idempotency_key":key}),
    )
}

fn derived(store: &mut Store, key: &str, ids: &[&str], body: &str) -> Value {
    let job = prepare(store, &format!("{key}-prepare"), ids);
    publish(store, &format!("{key}-publish"), &job, body)
}

fn recall(store: &mut Store) -> Value {
    call(store, "recall", json!({"limit":100,"max_chars":100_000}))
}

fn recalled_ids(result: &Value) -> Vec<String> {
    result["items"]
        .as_array()
        .expect("recall must contain items")
        .iter()
        .map(|record| record["id"].as_str().unwrap().to_owned())
        .collect()
}

fn assert_no_body(value: &Value, body: &str) {
    assert!(
        !value.to_string().contains(body),
        "public response exposed withdrawn body: {value}"
    );
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn response_loss_is_recoverable_without_repeating_the_mutation() {
    let (_directory, mut store) = database();
    let args = json!({"body":"receipt-recovery-marker","idempotency_key":"lost-response"});
    let original = call(&mut store, "remember", args.clone());
    let retried = call(&mut store, "remember", args);
    assert_eq!(retried, original);
    let recovered = call(
        &mut store,
        "operation",
        json!({"method":"remember","idempotency_key":"lost-response"}),
    );
    assert_eq!(recovered, original);
    assert_no_body(&recovered, "receipt-recovery-marker");
    assert_eq!(recalled_ids(&recall(&mut store)), vec![id(&original)]);
    assert_eq!(
        error_code(
            &mut store,
            NS,
            "operation",
            json!({"method":"remember","idempotency_key":"never-committed"})
        ),
        "not_found"
    );
}

#[test]
fn idempotency_conflicts_do_not_change_existing_records() {
    let (_directory, mut store) = database();
    let original = remember(&mut store, "same-key", "original-value");
    assert_eq!(
        error_code(
            &mut store,
            NS,
            "remember",
            json!({"body":"replacement-value","idempotency_key":"same-key"})
        ),
        "idempotency_conflict"
    );
    assert_eq!(
        call(&mut store, "inspect", json!({"id":id(&original)}))["body"],
        "original-value"
    );
    for key in [Value::Null, json!("")] {
        assert_eq!(
            error_code(
                &mut store,
                NS,
                "remember",
                json!({"body":"must-not-exist","idempotency_key":key})
            ),
            "invalid_request"
        );
    }
    assert_eq!(recalled_ids(&recall(&mut store)).len(), 1);
}

#[test]
fn namespace_is_pinned_outside_request_parameters() {
    let (_directory, mut store) = database();
    let secret = remember(&mut store, "shared-key", "namespace-a-secret");
    let other = store.execute(
        "other",
        "remember",
        json!({"body":"namespace-b-value","idempotency_key":"shared-key","namespace":NS}),
    );
    // Rejecting the unsolicited override or ignoring it are both safe outcomes.
    if let Ok(other) = other {
        assert_ne!(id(&secret), id(&other));
        let visible = store.execute("other", "recall", json!({})).unwrap();
        assert_no_body(&visible, "namespace-a-secret");
        assert!(recalled_ids(&visible).contains(&id(&other).to_owned()));
    }
    for method in ["inspect", "revise", "retract"] {
        let mut arguments = json!({"id":id(&secret)});
        if method != "inspect" {
            arguments["expected_revision"] = json!(1);
            arguments["idempotency_key"] = json!(format!("foreign-{method}"));
        }
        if method == "revise" {
            arguments["body"] = json!("tampered");
        }
        assert_eq!(
            error_code(&mut store, "other", method, arguments),
            "not_found"
        );
    }
    assert_eq!(
        error_code(
            &mut store,
            "other",
            "operation",
            json!({"method":"remember","idempotency_key":"shared-key-uncommitted"})
        ),
        "not_found"
    );
    assert_eq!(
        call(&mut store, "inspect", json!({"id":id(&secret)}))["body"],
        "namespace-a-secret"
    );
}

#[test]
fn jobs_cannot_cross_namespace_boundaries() {
    let (_directory, mut store) = database();
    let source = remember(&mut store, "source", "private-job-input");
    let job = prepare(&mut store, "prepared", &[id(&source)]);
    assert!(store
        .execute(
            "other",
            "prepare",
            json!({
                "inputs":[id(&source)],"processor":"fixture",
                "idempotency_key":"foreign-prepare"
            }),
        )
        .is_err());
    for method in ["job", "publish"] {
        let mut arguments = json!({"job_id":job["job_id"]});
        if method == "publish" {
            arguments["body"] = json!("foreign-output");
            arguments["idempotency_key"] = json!("foreign-publish");
        }
        assert_eq!(
            error_code(&mut store, "other", method, arguments),
            "not_found"
        );
    }
    assert_no_body(
        &store.execute("other", "export", json!({})).unwrap(),
        "private-job-input",
    );
}

#[test]
fn independent_connections_enforce_revision_compare_and_swap() {
    let (directory, mut first) = database();
    let original = remember(&mut first, "source", "old");
    let mut second = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    let winner = call(
        &mut first,
        "revise",
        json!({
            "id":id(&original),"expected_revision":1,"body":"winner",
            "idempotency_key":"first-writer"
        }),
    );
    assert_eq!(winner["revision"], 2);
    assert_eq!(
        error_code(
            &mut second,
            NS,
            "revise",
            json!({
                "id":id(&original),"expected_revision":1,"body":"lost-update",
                "idempotency_key":"second-writer"
            })
        ),
        "revision_conflict"
    );
    assert_eq!(
        call(&mut second, "inspect", json!({"id":id(&original)}))["body"],
        "winner"
    );
}

#[test]
fn simultaneous_writers_cannot_both_win_the_same_revision() {
    let (directory, mut store) = database();
    let original = remember(&mut store, "source", "initial");
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|writer| {
            let path = directory.path().join("memory.sqlite3");
            let source_id = id(&original).to_owned();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut connection = Store::open(path).unwrap();
                barrier.wait();
                connection.execute(
                    NS,
                    "revise",
                    json!({
                        "id":source_id,"expected_revision":1,
                        "body":format!("writer-{writer}"),
                        "idempotency_key":format!("writer-{writer}")
                    }),
                )
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    for result in results {
        if let Err(error) = result {
            assert_eq!(
                serde_json::to_value(error).unwrap()["code"],
                "revision_conflict"
            );
        }
    }
    let current = call(&mut store, "inspect", json!({"id":id(&original)}));
    assert_eq!(current["revision"], 2);
}

#[test]
fn revise_invalidates_three_derived_levels_but_keeps_unrelated_memory() {
    let (_directory, mut store) = database();
    let root = remember(&mut store, "root", "language-undecided");
    let unrelated = remember(&mut store, "unrelated", "project-uses-sqlite");
    let summary = derived(&mut store, "summary", &[id(&root)], "old-summary");
    let plan = derived(&mut store, "plan", &[id(&summary)], "old-plan");
    let decision = derived(&mut store, "decision", &[id(&plan)], "old-decision");
    call(
        &mut store,
        "revise",
        json!({
            "id":id(&root),"expected_revision":1,"body":"use-rust",
            "idempotency_key":"fix-root"
        }),
    );
    let visible = recalled_ids(&recall(&mut store));
    assert!(visible.contains(&id(&root).to_owned()));
    assert!(visible.contains(&id(&unrelated).to_owned()));
    for derived in [&summary, &plan, &decision] {
        assert!(!visible.contains(&id(derived).to_owned()));
        assert_eq!(
            error_code(&mut store, NS, "inspect", json!({"id":id(derived)})),
            "stale_dependency"
        );
    }
}

#[test]
fn late_job_cannot_publish_after_source_revision_or_retraction() {
    let (_directory, mut store) = database();
    let root = remember(&mut store, "root", "stale-job-input");
    let job = prepare(&mut store, "prepare-before-revision", &[id(&root)]);
    call(
        &mut store,
        "revise",
        json!({
            "id":id(&root),"expected_revision":1,"body":"new-job-input",
            "idempotency_key":"revision"
        }),
    );
    assert_eq!(
        error_code(
            &mut store,
            NS,
            "publish",
            json!({
                "job_id":job["job_id"],"body":"must-not-be-published",
                "idempotency_key":"late-revision-publish"
            })
        ),
        "stale_dependency"
    );
    let fresh = prepare(&mut store, "prepare-before-retract", &[id(&root)]);
    call(
        &mut store,
        "retract",
        json!({"id":id(&root),"expected_revision":2,"idempotency_key":"retract"}),
    );
    for job in [&job, &fresh] {
        assert!(store
            .execute(NS, "job", json!({"job_id":job["job_id"]}))
            .is_err());
        assert_eq!(
            error_code(
                &mut store,
                NS,
                "publish",
                json!({
                    "job_id":job["job_id"],"body":"must-not-be-published",
                    "idempotency_key":format!("late-retracted-{}",job["job_id"])
                })
            ),
            "stale_dependency"
        );
    }
    assert!(recalled_ids(&recall(&mut store)).is_empty());
}

#[test]
fn intact_alternative_input_does_not_authorize_old_free_text() {
    let (_directory, mut store) = database();
    let withdrawn = remember(&mut store, "a", "a-withdrawn-sensitive-detail");
    let intact = remember(&mut store, "b", "b-still-supports-some-claim");
    let mixed = derived(
        &mut store,
        "mixed",
        &[id(&withdrawn), id(&intact)],
        "mixed-output-containing-sensitive-detail",
    );
    let descendant = derived(
        &mut store,
        "nested",
        &[id(&mixed)],
        "nested-sensitive-detail",
    );
    call(
        &mut store,
        "retract",
        json!({"id":id(&withdrawn),"expected_revision":1,"idempotency_key":"withdraw-a"}),
    );
    let visible = recalled_ids(&recall(&mut store));
    assert_eq!(visible, vec![id(&intact)]);
    for record in [&mixed, &descendant] {
        assert_eq!(
            error_code(
                &mut store,
                NS,
                "inspect",
                json!({"id":id(record),"history":true})
            ),
            "not_found"
        );
    }
}

#[test]
fn revising_a_derived_record_does_not_erase_its_dependency() {
    let (_directory, mut store) = database();
    let root = remember(&mut store, "root", "root-detail");
    let summary = derived(&mut store, "summary", &[id(&root)], "summary-v1");
    call(
        &mut store,
        "revise",
        json!({
            "id":id(&summary),"expected_revision":1,"body":"summary-v2",
            "idempotency_key":"edit-derived"
        }),
    );
    call(
        &mut store,
        "retract",
        json!({"id":id(&root),"expected_revision":1,"idempotency_key":"withdraw-root"}),
    );
    assert_eq!(
        error_code(
            &mut store,
            NS,
            "inspect",
            json!({"id":id(&summary),"history":true})
        ),
        "not_found"
    );
    assert_eq!(
        error_code(
            &mut store,
            NS,
            "revise",
            json!({
                "id":id(&summary),"expected_revision":2,"body":"resurrection",
                "idempotency_key":"must-not-resurrect"
            })
        ),
        "not_found"
    );
}

#[test]
fn retraction_closes_history_export_jobs_and_receipt_body_paths() {
    let (_directory, mut store) = database();
    let root = remember(&mut store, "root", "erase-old-body-marker");
    call(
        &mut store,
        "revise",
        json!({
            "id":id(&root),"expected_revision":1,"body":"erase-current-body-marker",
            "idempotency_key":"root-revision"
        }),
    );
    let job = prepare(&mut store, "prepared", &[id(&root)]);
    let output = publish(&mut store, "published", &job, "erase-derived-body-marker");
    let pending = prepare(&mut store, "pending", &[id(&output)]);
    call(
        &mut store,
        "retract",
        json!({"id":id(&root),"expected_revision":2,"idempotency_key":"withdraw"}),
    );
    let mut public = vec![
        recall(&mut store),
        call(&mut store, "export", json!({})),
        call(&mut store, "status", json!({})),
    ];
    for (method, key) in [
        ("remember", "root"),
        ("revise", "root-revision"),
        ("prepare", "prepared"),
        ("publish", "published"),
        ("prepare", "pending"),
        ("retract", "withdraw"),
    ] {
        public.push(call(
            &mut store,
            "operation",
            json!({"method":method,"idempotency_key":key}),
        ));
    }
    for record in [&root, &output] {
        for history in [false, true] {
            let error = store
                .execute(NS, "inspect", json!({"id":id(record),"history":history}))
                .expect_err("retracted record was visible");
            public.push(serde_json::to_value(error).unwrap());
        }
    }
    for job in [&job, &pending] {
        let error = store
            .execute(NS, "job", json!({"job_id":job["job_id"]}))
            .expect_err("retracted job inputs were visible");
        public.push(serde_json::to_value(error).unwrap());
    }
    for response in public {
        for marker in [
            "erase-old-body-marker",
            "erase-current-body-marker",
            "erase-derived-body-marker",
        ] {
            assert_no_body(&response, marker);
        }
    }
}

#[test]
fn job_publication_is_single_use_but_exact_retry_is_recoverable() {
    let (_directory, mut store) = database();
    let root = remember(&mut store, "root", "input");
    let job = prepare(&mut store, "prepare", &[id(&root)]);
    let args = json!({
        "job_id":job["job_id"],"body":"one-output","idempotency_key":"publish-once"
    });
    let first = call(&mut store, "publish", args.clone());
    assert_eq!(call(&mut store, "publish", args), first);
    assert_eq!(
        error_code(
            &mut store,
            NS,
            "publish",
            json!({
                "job_id":job["job_id"],"body":"second-output",
                "idempotency_key":"different-publish"
            })
        ),
        "job_state_conflict"
    );
    assert_eq!(recalled_ids(&recall(&mut store)).len(), 2);
}

#[test]
fn temporal_bounds_filter_current_recall_and_preparation() {
    let (_directory, mut store) = database();
    let timestamp = now();
    let past = call(
        &mut store,
        "remember",
        json!({
            "body":"expired-body","valid_from":timestamp-3600,
            "valid_until":timestamp-60,"idempotency_key":"expired"
        }),
    );
    let future = call(
        &mut store,
        "remember",
        json!({
            "body":"future-body","valid_from":timestamp+3600,
            "idempotency_key":"future"
        }),
    );
    let current = call(
        &mut store,
        "remember",
        json!({
            "body":"current-body","valid_from":timestamp-60,
            "valid_until":timestamp+3600,"idempotency_key":"current"
        }),
    );
    assert_eq!(recalled_ids(&recall(&mut store)), vec![id(&current)]);
    for invalid in [&past, &future] {
        assert_eq!(
            error_code(
                &mut store,
                NS,
                "prepare",
                json!({
                    "inputs":[id(invalid)],"processor":"fixture",
                    "idempotency_key":format!("prepare-{}",id(invalid))
                })
            ),
            "stale_dependency"
        );
    }
}

#[test]
fn publish_checks_expiry_again_even_without_an_explicit_mutation() {
    let (_directory, mut store) = database();
    let expires = now() + 2;
    let root = call(
        &mut store,
        "remember",
        json!({
            "body":"short-lived-input","valid_until":expires,
            "idempotency_key":"expiring"
        }),
    );
    let job = prepare(&mut store, "prepared-before-expiry", &[id(&root)]);
    let summary = derived(&mut store, "short-lived-summary", &[id(&root)], "summary");
    let descendant_job = prepare(&mut store, "prepared-from-summary", &[id(&summary)]);
    while now() <= expires {
        std::thread::sleep(Duration::from_millis(20));
    }
    for job in [&job, &descendant_job] {
        assert_eq!(
            error_code(
                &mut store,
                NS,
                "publish",
                json!({
                    "job_id":job["job_id"],"body":"expired-result",
                    "idempotency_key":format!("publish-after-expiry-{}",job["job_id"])
                })
            ),
            "stale_dependency"
        );
    }
    assert_eq!(
        error_code(&mut store, NS, "inspect", json!({"id":id(&summary)})),
        "stale_dependency"
    );
    assert!(recalled_ids(&recall(&mut store)).is_empty());
}

#[test]
fn commit_receipts_cannot_be_used_for_another_database() {
    let (_a_directory, mut a) = database();
    let (_b_directory, mut b) = database();
    let record = remember(&mut a, "source", "stored-in-a");
    let token = record["commit_token"].as_str().unwrap();
    let same = call(&mut a, "recall", json!({"minimum_commit":token}));
    assert!(recalled_ids(&same).contains(&id(&record).to_owned()));
    assert_eq!(
        error_code(&mut b, NS, "recall", json!({"minimum_commit":token})),
        "commit_history_mismatch"
    );
}

#[test]
fn cjk_queries_and_character_budgets_preserve_unicode_and_isolation() {
    let (_directory, mut store) = database();
    let short = remember(&mut store, "short", "记忆🦀");
    remember(
        &mut store,
        "long",
        "记忆系统需要检查来源版本以及当前的适用范围",
    );
    store
        .execute(
            "other",
            "remember",
            json!({"body":"记忆跨空间机密","idempotency_key":"foreign"}),
        )
        .unwrap();
    let response = call(
        &mut store,
        "recall",
        json!({"query":"记忆","limit":10,"max_chars":3}),
    );
    let bodies: Vec<&str> = response["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["body"].as_str().unwrap())
        .collect();
    assert!(
        bodies
            .iter()
            .map(|body| body.chars().count())
            .sum::<usize>()
            <= 3
    );
    assert!(recalled_ids(&response).contains(&id(&short).to_owned()));
    assert_no_body(&response, "跨空间机密");
}

#[test]
fn literal_fts_queries_do_not_interpret_sql_or_fts_operators() {
    let (_directory, mut store) = database();
    let exact = remember(&mut store, "exact", "blueberry repair journal");
    remember(&mut store, "other", "unrelated notebook");
    let response = call(&mut store, "recall", json!({"query":"blueberry","limit":1}));
    assert_eq!(recalled_ids(&response), vec![id(&exact)]);
    for query in ["\"", "'; DROP TABLE memories; --", "OR", "NEAR(", "*"] {
        let _ = call(&mut store, "recall", json!({"query":query}));
    }
    assert_eq!(
        call(&mut store, "inspect", json!({"id":id(&exact)}))["body"],
        "blueberry repair journal"
    );
}

#[test]
fn restart_retains_records_jobs_retractions_and_idempotency_receipts() {
    let (directory, mut store) = database();
    let removed = remember(&mut store, "removed", "do-not-resurrect-after-restart");
    let live = remember(&mut store, "live", "live-after-restart");
    let job = prepare(&mut store, "pending", &[id(&live)]);
    call(
        &mut store,
        "retract",
        json!({"id":id(&removed),"expected_revision":1,"idempotency_key":"withdraw"}),
    );
    drop(store);
    let mut reopened = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    assert_eq!(recalled_ids(&recall(&mut reopened)), vec![id(&live)]);
    let result = publish(
        &mut reopened,
        "complete-after-restart",
        &job,
        "fresh-summary",
    );
    assert_eq!(result["revision"], 1);
    let receipt = call(
        &mut reopened,
        "operation",
        json!({"method":"remember","idempotency_key":"removed"}),
    );
    assert_eq!(receipt, removed);
    assert_no_body(&receipt, "do-not-resurrect-after-restart");
    let retried = remember(&mut reopened, "removed", "do-not-resurrect-after-restart");
    assert_eq!(retried, removed);
    assert_eq!(
        error_code(
            &mut reopened,
            NS,
            "inspect",
            json!({"id":id(&removed),"history":true})
        ),
        "not_found"
    );
    assert_no_body(
        &call(&mut reopened, "export", json!({})),
        "do-not-resurrect-after-restart",
    );
}
