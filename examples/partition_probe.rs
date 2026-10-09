//! A hand-constructed dependency-grouping mechanism example.
//!
//! Run with `cargo run --example partition_probe`.
//! This is not a performance benchmark, paper reproduction, or evidence of
//! semantic/task improvement. It uses no model and makes no token-cost estimate.
//! Only fixture timestamps are set through SQL to establish an explicit source
//! order. All memory creation, derivation, correction and visibility checks use
//! the public Store API.

use recall_ledger::Store;
use serde_json::{json, Value};
use std::collections::HashSet;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const NAMESPACE: &str = "partition-probe";
const RECORD_COUNT: usize = 12;
const RECORD_CHARS: usize = 16;
const GROUP_CHARS: usize = 4 * RECORD_CHARS;

fn execute(store: &mut Store, method: &str, params: Value) -> Result<Value> {
    Ok(store.execute(NAMESPACE, method, params)?)
}

fn observe(strategy: &str, all_sources_change: bool) -> Result<Value> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("fixture.db");
    let mut store = Store::open(&database)?;
    let mut sources = Vec::new();
    for index in 0..RECORD_COUNT {
        let body = format!("item-{index:02}-payload!");
        assert_eq!(body.chars().count(), RECORD_CHARS);
        let record = execute(
            &mut store,
            "remember",
            json!({
                "body": body,
                "source": "same-fixture-source",
                "kind": "episode",
                "volatility": if index % 2 == 0 { "stable" } else { "volatile" },
                "idempotency_key": format!("seed-{index}")
            }),
        )?;
        sources.push(record["id"].as_str().unwrap().to_owned());
    }
    {
        // Test-fixture construction only. This removes second-resolution clock
        // ties and random UUID ordering; it is not a supported production API.
        // The fixed planner will see stable, volatile, stable, volatile, ... .
        let connection = rusqlite::Connection::open(&database)?;
        for (index, id) in sources.iter().enumerate() {
            connection.execute(
                "UPDATE records SET updated_at=?1 WHERE id=?2",
                rusqlite::params![10_000 - index as i64, id],
            )?;
        }
    }
    let plan = execute(
        &mut store,
        "plan",
        json!({"strategy": strategy, "max_group_chars": GROUP_CHARS}),
    )?;
    let groups = plan["groups"].as_array().unwrap();
    assert!(plan["oversized"].as_array().unwrap().is_empty());
    let mut artifacts = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        let prepared = execute(
            &mut store,
            "prepare",
            json!({
                "inputs": group["inputs"],
                "processor": "fixture-concatenation/v1",
                "idempotency_key": format!("prepare-{index}")
            }),
        )?;
        let job = execute(&mut store, "job", json!({"job_id": prepared["job_id"]}))?;
        // Verbatim concatenation exercises dependency tracking without claiming
        // model compression, summary accuracy or equal generation work.
        let body = job["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["body"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let payload_chars = body.chars().count();
        let published = execute(
            &mut store,
            "publish",
            json!({
                "job_id": prepared["job_id"],
                "body": body,
                "idempotency_key": format!("publish-{index}")
            }),
        )?;
        artifacts.push((
            published["id"].as_str().unwrap().to_owned(),
            group["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            payload_chars,
        ));
    }
    let changed: HashSet<_> = sources
        .iter()
        .enumerate()
        .filter(|(index, _)| all_sources_change || index % 2 == 1)
        .map(|(_, id)| id.clone())
        .collect();
    for (index, id) in sources.iter().enumerate() {
        if changed.contains(id) {
            execute(
                &mut store,
                "revise",
                json!({
                    "id": id,
                    "expected_revision": 1,
                    "body": format!("next-{index:02}-payload!"),
                    "idempotency_key": format!("revise-{index}")
                }),
            )?;
        }
    }
    // Count unique artifacts after the entire batch, not the sum of individual
    // invalidated_descendants receipts, which could count the same artifact twice.
    let mut invalidated = 0;
    let mut invalidated_payload_chars = 0;
    let mut collateral_sources = HashSet::new();
    for (id, inputs, payload_chars) in &artifacts {
        match store.execute(NAMESPACE, "inspect", json!({"id": id})) {
            Ok(_) => {}
            Err(error) if error.code == "stale_dependency" => {
                invalidated += 1;
                invalidated_payload_chars += payload_chars;
                for input in inputs {
                    if !changed.contains(input) {
                        collateral_sources.insert(input);
                    }
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(json!({
        "strategy": strategy,
        "max_group_chars": GROUP_CHARS,
        "initial_groups_and_derived_records": artifacts.len(),
        "initial_derived_payload_chars": artifacts.iter().map(|(_, _, chars)| chars).sum::<usize>(),
        "changed_sources": changed.len(),
        "invalidated_derived_records": invalidated,
        "remaining_readable_derived_records": artifacts.len() - invalidated,
        "invalidated_derived_payload_chars": invalidated_payload_chars,
        "unchanged_sources_in_invalidated_groups": collateral_sources.len(),
        "group_input_counts": groups.iter().map(|group| group["inputs"].as_array().unwrap().len()).collect::<Vec<_>>()
    }))
}

pub fn run_probe() -> Result<Value> {
    let mut workloads = Vec::new();
    for all_sources_change in [false, true] {
        let mut rows = Vec::new();
        for strategy in ["individual", "fixed", "volatility"] {
            rows.push(observe(strategy, all_sources_change)?);
        }
        workloads.push(json!({
            "workload": if all_sources_change { "all_sources_change_counterexample" } else { "volatile_sources_change" },
            "results": rows
        }));
    }
    Ok(json!({
        "classification": "hand-constructed mechanism example",
        "not_evidence_of": ["performance", "paper reproduction", "semantic or task gain", "token cost", "learned volatility prediction"],
        "fixture": {
            "records": RECORD_COUNT,
            "unicode_scalars_per_record": RECORD_CHARS,
            "order": "stable and volatile records alternate",
            "order_control": "fixture-only SQL assigns explicit updated_at values; UUID order is irrelevant",
            "derivation": "verbatim concatenation through real prepare/job/publish calls",
            "mutation": "public revise calls; unique stale artifacts counted after the complete batch",
            "same_max_group_chars_all_strategies": GROUP_CHARS
        },
        "cost_boundary": "The character cap is equal; total generation counts and initial derived-record counts are not equal. Invalidated records are not token costs. No rebuild, model, retrieval-quality or latency benchmark is run.",
        "counterexample": "When all sources change, volatility partitioning invalidates four artifacts versus three for fixed grouping. There is no universal advantage.",
        "workloads": workloads
    }))
}

// This file is also included by planning integration tests to verify the exact
// executable example against explicit expected observations.
#[allow(dead_code)]
fn main() -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&run_probe()?)?);
    Ok(())
}
