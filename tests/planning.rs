//! Plan boundaries and an explicit, model-free invalidation mechanism fixture.

#[path = "../examples/partition_probe.rs"]
mod partition_probe;

use recall_ledger::Store;
use serde_json::json;
use std::collections::{HashMap, HashSet};

fn remember(
    store: &mut Store,
    namespace: &str,
    key: &str,
    body: &str,
    source: &str,
    kind: &str,
) -> String {
    store
        .execute(
            namespace,
            "remember",
            json!({
                "body": body, "source": source, "kind": kind,
                "idempotency_key": key
            }),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn constructed_batch_changes_show_tradeoff_and_all_change_counterexample() {
    let probe = partition_probe::run_probe().unwrap();
    assert_eq!(
        probe["classification"],
        "hand-constructed mechanism example"
    );
    let targeted = &probe["workloads"][0]["results"];
    let all = &probe["workloads"][1]["results"];
    // All methods have the same input character cap, but differing construction
    // counts. Never report these counts as equal-total-cost algorithm gains.
    for (index, initial) in [12, 3, 4].into_iter().enumerate() {
        assert_eq!(targeted[index]["max_group_chars"], 64);
        assert_eq!(all[index]["max_group_chars"], 64);
        assert_eq!(
            targeted[index]["initial_groups_and_derived_records"],
            initial
        );
        assert_eq!(all[index]["initial_groups_and_derived_records"], initial);
        assert_eq!(all[index]["invalidated_derived_records"], initial);
        assert_eq!(all[index]["remaining_readable_derived_records"], 0);
    }
    assert_eq!(targeted[0]["invalidated_derived_records"], 6);
    assert_eq!(targeted[1]["invalidated_derived_records"], 3);
    assert_eq!(targeted[2]["invalidated_derived_records"], 2);
    assert_eq!(targeted[0]["unchanged_sources_in_invalidated_groups"], 0);
    assert_eq!(targeted[1]["unchanged_sources_in_invalidated_groups"], 6);
    assert_eq!(targeted[2]["unchanged_sources_in_invalidated_groups"], 0);
    assert!(
        all[2]["invalidated_derived_records"].as_u64().unwrap()
            > all[1]["invalidated_derived_records"].as_u64().unwrap()
    );
}

#[test]
fn every_strategy_preserves_namespace_source_and_kind_boundaries() {
    let mut store = Store::open(":memory:").unwrap();
    let mut labels = HashMap::new();
    for (index, (source, kind)) in [
        ("source-a", "episode"),
        ("source-a", "episode"),
        ("source-b", "episode"),
        ("source-b", "episode"),
        ("source-a", "preference"),
        ("source-a", "preference"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = remember(
            &mut store,
            "workspace-a",
            &format!("seed-{index}"),
            "same body",
            source,
            kind,
        );
        labels.insert(id, (source, kind));
    }
    let foreign = remember(
        &mut store,
        "workspace-b",
        "foreign",
        "same body",
        "source-a",
        "episode",
    );
    for strategy in ["individual", "fixed", "topic", "volatility"] {
        let plan = store
            .execute(
                "workspace-a",
                "plan",
                json!({"strategy": strategy, "max_group_chars": 1000}),
            )
            .unwrap();
        let mut covered = HashSet::new();
        for group in plan["groups"].as_array().unwrap() {
            let ids = group["inputs"].as_array().unwrap();
            let first = labels[ids[0].as_str().unwrap()];
            for id in ids {
                let id = id.as_str().unwrap();
                assert_ne!(id, foreign);
                assert_eq!(labels[id], first, "source or kind crossed");
                assert!(covered.insert(id.to_owned()), "duplicate planned input");
            }
        }
        assert_eq!(covered.len(), labels.len());
        assert!(plan["oversized"].as_array().unwrap().is_empty());
    }
}

#[test]
fn unicode_character_budget_and_oversized_records_are_explicit() {
    let mut store = Store::open(":memory:").unwrap();
    let mut lengths = HashMap::new();
    for (index, body) in ["汉字", "ab", "é🙂", "12345"].into_iter().enumerate() {
        let id = remember(
            &mut store,
            "budget",
            &format!("seed-{index}"),
            body,
            "same",
            "episode",
        );
        lengths.insert(id, body.chars().count());
    }
    let oversized = lengths
        .iter()
        .find(|(_, length)| **length == 5)
        .unwrap()
        .0
        .clone();
    for strategy in ["individual", "fixed", "topic", "volatility"] {
        let plan = store
            .execute(
                "budget",
                "plan",
                json!({"strategy": strategy, "max_group_chars": 4}),
            )
            .unwrap();
        assert_eq!(plan["oversized"], json!([oversized]));
        let mut covered = HashSet::new();
        for group in plan["groups"].as_array().unwrap() {
            let sum: usize = group["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| {
                    let id = id.as_str().unwrap();
                    assert_ne!(id, oversized);
                    assert!(covered.insert(id.to_owned()));
                    lengths[id]
                })
                .sum();
            assert!(sum <= 4);
            assert_eq!(group["chars"], sum);
        }
        assert_eq!(covered.len(), 3);
    }
}

#[test]
fn input_cap_splits_groups_even_when_character_budget_allows_more() {
    let mut store = Store::open(":memory:").unwrap();
    let mut ids = Vec::new();
    for index in 0..65 {
        ids.push(remember(
            &mut store,
            "cap",
            &format!("seed-{index}"),
            "x",
            "same",
            "episode",
        ));
    }
    for strategy in ["fixed", "topic", "volatility"] {
        let plan = store
            .execute(
                "cap",
                "plan",
                json!({"strategy": strategy, "max_group_chars": 1000}),
            )
            .unwrap();
        let mut sizes: Vec<_> = plan["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|group| group["inputs"].as_array().unwrap().len())
            .collect();
        sizes.sort();
        assert_eq!(sizes, [1, 64]);
        for (index, group) in plan["groups"].as_array().unwrap().iter().enumerate() {
            let prepared = store
                .execute(
                    "cap",
                    "prepare",
                    json!({
                        "inputs": group["inputs"], "processor": "cap-check",
                        "idempotency_key": format!("{strategy}-{index}")
                    }),
                )
                .unwrap();
            assert_eq!(prepared["state"], "prepared");
        }
    }
    let rejected = store
        .execute(
            "cap",
            "prepare",
            json!({"inputs": ids, "processor": "cap-check", "idempotency_key": "oversized-prepare"}),
        )
        .unwrap_err();
    assert_eq!(rejected.code, "invalid_request");
    let individual = store
        .execute(
            "cap",
            "plan",
            json!({"strategy": "individual", "max_group_chars": 1000}),
        )
        .unwrap();
    assert_eq!(individual["groups"].as_array().unwrap().len(), 65);
}
