//! Runtime execution against temporary real SQLite stores; no external models.
use recall_ledger::models::{Config, SummaryConfig};
use recall_ledger::runtime::Runtime;
use recall_ledger::Store;
use serde_json::{json, Value};
use std::net::TcpListener;
use tempfile::TempDir;

const NS: &str = "fidelity-runtime";
const PROCESSOR: &str = "recall-ledger.evidence/1";

fn database(config: Option<Config>) -> (TempDir, Runtime) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    (directory, Runtime::new(store, config).unwrap())
}

fn call(runtime: &mut Runtime, method: &str, args: Value) -> Value {
    runtime
        .execute(NS, method, args)
        .unwrap_or_else(|error| panic!("{method} failed: {error}"))
}

fn remember(runtime: &mut Runtime, key: &str, body: &str) -> Value {
    call(
        runtime,
        "remember",
        json!({"body":body,"idempotency_key":key}),
    )
}

fn consolidate(runtime: &mut Runtime, key: &str, id: &Value) -> Value {
    call(
        runtime,
        "consolidate",
        json!({"inputs":[id],"idempotency_key":key}),
    )
}

fn tick(runtime: &mut Runtime, maximum: usize) -> Value {
    call(
        runtime,
        "maintain",
        json!({"max_embeddings":0,"max_summaries":maximum,"auto_summary_min":0}),
    )
}

fn task<'a>(status: &'a Value, recipe: &Value) -> &'a Value {
    status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["recipe_id"] == recipe["recipe_id"])
        .unwrap()
}

#[test]
fn no_model_capsule_preserves_qualifiers_and_repairs_then_withdraws() {
    let (_directory, mut runtime) = database(None);
    let text = "仅本地部署；未获确认时不得发送。\n不是所有 Agent 都支持此功能。";
    let source = remember(&mut runtime, "source", text);
    let recipe = consolidate(&mut runtime, "capsule", &source["id"]);
    let expected = call(
        &mut runtime,
        "evidence_sources",
        json!({"inputs":[source["id"]]}),
    );
    let maintained = tick(&mut runtime, 1);
    assert_eq!(maintained["processed"], json!({"evidence":1,"model":0}));
    assert_eq!(maintained["errors"], json!([]));
    let first = call(
        &mut runtime,
        "evidence_inspect",
        json!({"id":recipe["output_id"]}),
    );
    assert_eq!(first["verified"], true);
    assert_eq!(first["body"], expected["body"]);
    assert_eq!(first["roots"][0]["body"], text);
    assert_eq!(first["fidelity"]["semantics_verified"], false);
    assert_eq!(first["coverage"]["truncated"], false);
    let old_context = call(&mut runtime, "recall", json!({"max_chars":16000}));
    call(
        &mut runtime,
        "revise",
        json!({
            "id":source["id"],"expected_revision":1,
            "body":"仅在用户明确授权后发送；仍然要求本地部署。",
            "idempotency_key":"correct-source"
        }),
    );
    assert!(!call(
        &mut runtime,
        "validate_context",
        json!({"manifest":old_context["manifest"]})
    )["valid"]
        .as_bool()
        .unwrap());
    assert!(runtime
        .execute(NS, "inspect", json!({"id":recipe["output_id"]}))
        .is_err());
    tick(&mut runtime, 1);
    let repaired = call(
        &mut runtime,
        "evidence_inspect",
        json!({"id":recipe["output_id"]}),
    );
    assert_eq!(repaired["revision"], 2);
    assert_eq!(repaired["roots"][0]["revision"], 2);
    assert_eq!(
        repaired["roots"][0]["body"],
        "仅在用户明确授权后发送；仍然要求本地部署。"
    );
    assert!(!repaired.to_string().contains("不是所有 Agent"));
    call(
        &mut runtime,
        "retract",
        json!({
            "id":source["id"],"expected_revision":2,
            "idempotency_key":"withdraw-source"
        }),
    );
    let result = tick(&mut runtime, 1);
    assert_eq!(result["processed"]["evidence"], 0);
    assert!(runtime
        .execute(NS, "evidence_inspect", json!({"id":recipe["output_id"]}))
        .is_err());
    assert!(!call(&mut runtime, "recall", json!({}))
        .to_string()
        .contains("明确授权后发送"));
}

#[test]
fn automatic_default_is_exact_even_when_a_model_is_configured() {
    // A bound, unserved loopback listener detects even an attempted HTTP call.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = Config {
        embedding: None,
        summary: Some(SummaryConfig {
            url: format!(
                "http://{}/v1/chat/completions",
                listener.local_addr().unwrap()
            ),
            model: "must-not-run".into(),
            revision: "fixture".into(),
            timeout_seconds: 1,
            max_tokens: 128,
            extra_body: None,
        }),
    };
    let (_directory, mut runtime) = database(Some(config));
    for index in 0..4 {
        remember(
            &mut runtime,
            &format!("observation-{index}"),
            &format!("Observation {index} is conditional, not a universal rule."),
        );
    }
    let first = call(&mut runtime, "maintain", json!({"max_embeddings":0}));
    assert_eq!(first["summary_policy"], "evidence");
    assert_eq!(first["max_summary_chars"], 16384);
    assert_eq!(first["processed"], json!({"evidence":1,"model":0}));
    assert_eq!(first["errors"], json!([]));
    assert_eq!(first["actions"][0]["kind"], "evidence_queued");
    assert_eq!(first["actions"][1]["kind"], "evidence_published");
    assert_eq!(first["actions"][1]["fidelity_contract"], "source_exact");
    let second = call(&mut runtime, "maintain", json!({"max_embeddings":0}));
    assert_eq!(second["processed"], json!({"evidence":0,"model":0}));
    assert_eq!(second["actions"], json!([]));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "strict automatic consolidation must never contact a model"
    );
}

#[test]
fn exact_queue_precedes_legacy_and_unavailable_processors_remain_pending() {
    let (_directory, mut runtime) = database(None);
    let source = remember(&mut runtime, "source", "Synthetic source.");
    let legacy = call(
        &mut runtime,
        "summary_create",
        json!({
            "inputs":[source["id"]],"processor":"different-model",
            "idempotency_key":"older-legacy"
        }),
    );
    let strict = consolidate(&mut runtime, "newer-evidence", &source["id"]);
    let result = tick(&mut runtime, 1);
    assert_eq!(result["processed"], json!({"evidence":1,"model":0}));
    assert_eq!(result["errors"], json!([]));
    assert_eq!(task(&result["maintenance"], &strict)["state"], "succeeded");
    let waiting = task(&result["maintenance"], &legacy);
    assert_eq!(waiting["state"], "pending");
    assert_eq!(waiting["claim_count"], 0);
    assert_eq!(waiting["runtime_processor_available"], false);
    assert_eq!(waiting["fidelity_contract"], "unchecked");
    assert_eq!(
        waiting["waiting_reason"],
        "matching_model_configuration_required"
    );
    assert_eq!(tick(&mut runtime, 1)["processed"]["evidence"], 0);
}

#[test]
fn strict_jobs_survive_restart_and_obey_one_shared_task_budget() {
    let (directory, mut runtime) = database(None);
    for index in 0..2 {
        let source = remember(&mut runtime, &format!("s-{index}"), "Source.");
        consolidate(&mut runtime, &format!("c-{index}"), &source["id"]);
    }
    assert_eq!(tick(&mut runtime, 0)["processed"]["evidence"], 0);
    drop(runtime);
    let store = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    let mut runtime = Runtime::new(store, None).unwrap();
    assert_eq!(tick(&mut runtime, 1)["processed"]["evidence"], 1);
    let status = call(&mut runtime, "maintenance_status", json!({}));
    assert_eq!(status["counts"]["succeeded"], 1);
    assert_eq!(status["counts"]["pending"], 1);
    assert_eq!(tick(&mut runtime, 1)["processed"]["evidence"], 1);
    assert_eq!(tick(&mut runtime, 1)["processed"]["evidence"], 0);
}

#[test]
fn grown_source_fails_closed_without_blocking_other_strict_work() {
    let (_directory, mut runtime) = database(None);
    let source = remember(&mut runtime, "source", "Short.");
    let capsule = call(
        &mut runtime,
        "consolidate",
        json!({"inputs":[source["id"]],"max_chars":2000,"idempotency_key":"bounded"}),
    );
    tick(&mut runtime, 1);
    call(
        &mut runtime,
        "revise",
        json!({
            "id":source["id"],"expected_revision":1,"body":"文".repeat(2400),
            "idempotency_key":"grow"
        }),
    );
    let other = remember(&mut runtime, "other-source", "Other source fits.");
    let next = consolidate(&mut runtime, "next", &other["id"]);
    let result = tick(&mut runtime, 1);
    assert!(result["actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["kind"] == "maintenance_queue_scan" && action["failed_tasks"] == 1));
    assert_eq!(task(&result["maintenance"], &capsule)["state"], "failed");
    assert_eq!(
        task(&result["maintenance"], &capsule)["error_code"],
        "resource_exhausted"
    );
    assert_eq!(task(&result["maintenance"], &next)["state"], "succeeded");
    assert_eq!(result["processed"]["evidence"], 1);
    assert!(runtime
        .execute(NS, "inspect", json!({"id":capsule["output_id"]}))
        .is_err());
    let after = tick(&mut runtime, 1);
    assert_eq!(after["processed"]["evidence"], 0);
    assert_eq!(
        task(&after["maintenance"], &capsule)["claim_count"],
        task(&result["maintenance"], &capsule)["claim_count"]
    );
}

#[test]
fn model_policy_still_repairs_existing_strict_jobs_without_a_provider() {
    let (_directory, mut runtime) = database(None);
    let source = remember(&mut runtime, "source", "Source still needs repair.");
    consolidate(&mut runtime, "strict", &source["id"]);
    let result = call(
        &mut runtime,
        "maintain",
        json!({"summary_policy":"model","max_embeddings":0}),
    );
    assert_eq!(result["max_summary_chars"], 2000);
    assert_eq!(result["processed"], json!({"evidence":1,"model":0}));
    assert_eq!(
        result["errors"],
        json!([{"kind":"summary_queue","code":"model_not_configured"}])
    );
}

#[test]
fn policies_have_explicit_bounds_and_consolidate_receipts_are_queryable() {
    let (_directory, mut runtime) = database(None);
    let source = remember(&mut runtime, "source", "Source.");
    let receipt = consolidate(&mut runtime, "receipt", &source["id"]);
    assert_eq!(receipt["receipt_method"], "evidence_create");
    let queried = call(
        &mut runtime,
        "operation",
        json!({"method":"consolidate","idempotency_key":"receipt"}),
    );
    assert!(queried
        .to_string()
        .contains(receipt["recipe_id"].as_str().unwrap()));
    for args in [
        json!({"summary_policy":"unknown"}),
        json!({"summary_policy":"model","max_summary_chars":16385}),
        json!({"summary_policy":"evidence","max_summary_chars":65537}),
        json!({"max_summaries":9}),
    ] {
        assert_eq!(
            runtime.execute(NS, "maintain", args).unwrap_err().code,
            "invalid_request"
        );
    }
    let result = call(
        &mut runtime,
        "maintain",
        json!({
            "summary_policy":"evidence","max_summary_chars":65536,
            "auto_summary_min":0,"max_summaries":0
        }),
    );
    assert_eq!(result["max_summary_chars"], 65536);
}

#[test]
fn stale_intermediate_remains_a_barrier_until_its_owner_repairs_it() {
    let (_directory, mut runtime) = database(None);
    let root = remember(
        &mut runtime,
        "root",
        "Conditional source: only on Tuesdays.",
    );
    let legacy = call(
        &mut runtime,
        "summary_create",
        json!({
            "inputs":[root["id"]],"processor":"external-owner",
            "idempotency_key":"legacy"
        }),
    );
    let claim = call(
        &mut runtime,
        "maintenance_claim",
        json!({
            "worker":"fixture-owner","processor":"external-owner",
            "idempotency_key":"claim-legacy"
        }),
    );
    call(
        &mut runtime,
        "maintenance_complete",
        json!({
            "task_id":claim["task_id"],"lease_token":claim["lease_token"],
            "body":"Unchecked wording says every day.","idempotency_key":"publish-legacy"
        }),
    );
    let strict = consolidate(&mut runtime, "strict", &legacy["output_id"]);
    tick(&mut runtime, 1);
    let result = call(
        &mut runtime,
        "evidence_inspect",
        json!({"id":strict["output_id"]}),
    );
    assert_eq!(result["roots"][0]["id"], root["id"]);
    assert_eq!(result["dependencies"].as_array().unwrap().len(), 2);
    assert!(!result["body"].as_str().unwrap().contains("every day"));
    call(
        &mut runtime,
        "revise",
        json!({
            "id":root["id"],"expected_revision":1,"body":"Only on Thursdays.",
            "idempotency_key":"new-day"
        }),
    );
    let waiting = tick(&mut runtime, 1);
    assert_eq!(waiting["processed"]["evidence"], 0);
    assert!(runtime
        .execute(NS, "inspect", json!({"id":strict["output_id"]}))
        .is_err());
    let claim = call(
        &mut runtime,
        "maintenance_claim",
        json!({
            "worker":"fixture-owner","processor":"external-owner",
            "idempotency_key":"claim-repair"
        }),
    );
    assert_eq!(claim["claimed"], true);
    call(
        &mut runtime,
        "maintenance_complete",
        json!({
            "task_id":claim["task_id"],"lease_token":claim["lease_token"],
            "body":"Repaired owner output, still unchecked.",
            "idempotency_key":"publish-repair"
        }),
    );
    assert_eq!(tick(&mut runtime, 1)["processed"]["evidence"], 1);
    let repaired = call(
        &mut runtime,
        "evidence_inspect",
        json!({"id":strict["output_id"]}),
    );
    assert_eq!(repaired["roots"][0]["body"], "Only on Thursdays.");
    assert_eq!(repaired["fidelity"]["canonical_format"], PROCESSOR);
}
