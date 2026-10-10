//! Independent cross-module counterexamples for the v0.3 local contract.
//! Text, vectors and processor outputs are synthetic; no model is invoked.
use recall_ledger::{projection::read_projection, Store};
use serde_json::{json, Value};
use std::fs;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NS: &str = "adversarial";

fn invoke(store: &mut Store, method: &str, params: Value) -> Value {
    store.execute(NS, method, params).unwrap()
}

fn fact(conditions: &str) -> Value {
    json!({
        "entity":"project:example",
        "attribute":"launch",
        "scope":"repository:example",
        "conditions":conditions
    })
}

fn submission(event: &str, body: &str, key: Value) -> Value {
    json!({
        "event_id":event,"body":body,"fact_key":key,
        "origin":"user_explicit","source":"synthetic-review",
        "idempotency_key":format!("submit-{event}")
    })
}

fn submit(store: &mut Store, event: &str, body: &str) -> Value {
    invoke(
        store,
        "candidate_submit",
        submission(event, body, fact("default")),
    )
}

fn accept(store: &mut Store, candidate: &Value, key: &str) -> Value {
    invoke(
        store,
        "candidate_resolve",
        json!({
            "candidate_id":candidate["candidate_id"],
            "decision":"accept","idempotency_key":key
        }),
    )
}

fn all_context(store: &mut Store) -> Value {
    invoke(
        store,
        "recall",
        json!({"query":"","limit":100,"max_chars":10000}),
    )
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn accepted_correction_fences_derivations_vectors_and_precompiled_context_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("live.db")).unwrap();
    let first = submit(&mut store, "first", "OLD_SYNTHETIC_APRIL");
    let accepted = accept(&mut store, &first, "accept-first");
    let id = accepted["record_id"].clone();
    invoke(
        &mut store,
        "embedding_set",
        json!({"id":id,"expected_revision":1,"model_key":"synthetic",
               "vector":[1.0,0.0],"idempotency_key":"vector-first"}),
    );
    let summary = invoke(
        &mut store,
        "summary_create",
        json!({"inputs":[id],"processor":"synthetic","max_chars":1000,
               "idempotency_key":"summary"}),
    );
    let lease = invoke(
        &mut store,
        "maintenance_claim",
        json!({"worker":"synthetic","lease_seconds":60,"idempotency_key":"claim-1"}),
    );
    let published = invoke(
        &mut store,
        "maintenance_complete",
        json!({"task_id":summary["task_id"],"lease_token":lease["lease_token"],
               "body":"OLD_SUMMARY_APRIL","idempotency_key":"complete-1"}),
    );
    let pending_manual = invoke(
        &mut store,
        "prepare",
        json!({"inputs":[published["id"]],"processor":"synthetic-second-level",
               "idempotency_key":"prepare-before-correction"}),
    );
    let context = all_context(&mut store);
    assert_eq!(context["items"].as_array().unwrap().len(), 2);
    let proposal = submit(&mut store, "correction", "NEW_SYNTHETIC_AUGUST");
    let before_failure = invoke(&mut store, "status", json!({}))["commit_token"].clone();

    // A failing admission must leave neither a mutation event nor a partial update.
    assert_eq!(
        store
            .execute(
                NS,
                "candidate_resolve",
                json!({"candidate_id":proposal["candidate_id"],"decision":"accept",
                       "expected_revision":999,"idempotency_key":"correction-cas"}),
            )
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    assert_eq!(
        invoke(&mut store, "changes", json!({"after":before_failure}))["events"],
        json!([])
    );
    assert_eq!(
        invoke(&mut store, "fact_get", json!({"fact_key":fact("default")}))["revision"],
        1
    );
    let corrected = invoke(
        &mut store,
        "candidate_resolve",
        json!({"candidate_id":proposal["candidate_id"],"decision":"accept",
               "expected_revision":1,"idempotency_key":"correction-cas"}),
    );
    assert_eq!(corrected["record_id"], id);
    assert_eq!(corrected["revision"], 2);
    let feed = invoke(&mut store, "changes", json!({"after":before_failure}));
    assert_eq!(feed["events"].as_array().unwrap().len(), 1);
    assert_eq!(feed["events"][0]["method"], "candidate_resolve");
    assert_eq!(feed["events"][0]["record_ids"], json!([id]));
    assert_eq!(feed["record_ids_complete"], false);
    assert_eq!(feed["resync_required"], true);
    assert!(!feed.to_string().contains("SYNTHETIC"));
    let validation = invoke(
        &mut store,
        "validate_context",
        json!({"manifest":context["manifest"]}),
    );
    assert_eq!(validation["valid"], false);
    assert_eq!(validation["invalid"].as_array().unwrap().len(), 2);
    assert_eq!(
        store
            .execute(
                NS,
                "publish",
                json!({"job_id":pending_manual["job_id"],"body":"late old conclusion",
                       "idempotency_key":"late-manual"}),
            )
            .unwrap_err()
            .code,
        "stale_dependency"
    );
    let semantic = invoke(
        &mut store,
        "recall",
        json!({"mode":"vector","model_key":"synthetic","query_vector":[1.0,0.0]}),
    );
    assert_eq!(semantic["items"], json!([]));
    let old_candidate = invoke(
        &mut store,
        "candidate_inspect",
        json!({"candidate_id":first["candidate_id"]}),
    );
    assert_eq!(old_candidate["body"], "NEW_SYNTHETIC_AUGUST");
    assert_eq!(old_candidate["accepted_revision"], 1);
    assert_eq!(old_candidate["current_revision"], 2);
    assert_eq!(old_candidate["record_changed_since_acceptance"], true);
    let directory = temp.path().join("view");
    store.project(NS, &directory).unwrap();
    let projected = read_projection(directory).unwrap();
    assert!(projected["body"]
        .as_str()
        .unwrap()
        .contains("NEW_SYNTHETIC_AUGUST"));
    assert!(!projected["body"].as_str().unwrap().contains("OLD_"));
}

#[test]
fn withdrawal_survives_restore_and_old_candidate_event_replay_without_body_resurrection() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("live.db")).unwrap();
    let original_request = submission("original", "SENSITIVE_SYNTHETIC_ORIGINAL", fact("default"));
    let original = invoke(&mut store, "candidate_submit", original_request.clone());
    let accepted = accept(&mut store, &original, "accept-original");
    let waiting = submit(&mut store, "pending", "SENSITIVE_SYNTHETIC_PENDING");
    let context = all_context(&mut store);
    invoke(
        &mut store,
        "retract",
        json!({"id":accepted["record_id"],"expected_revision":1,"idempotency_key":"withdraw"}),
    );
    let old_cursor = invoke(&mut store, "changes", json!({}))["cursor"].clone();
    let backup = temp.path().join("backup.db");
    store.backup(&backup).unwrap();
    let destination = temp.path().join("restored.db");
    let recovery = Store::restore(&backup, &destination).unwrap();
    let mut restored = Store::open(destination).unwrap();
    assert_ne!(recovery["commit_token"], old_cursor);
    assert_eq!(
        restored
            .execute(NS, "changes", json!({"after":old_cursor}))
            .unwrap_err()
            .code,
        "commit_history_mismatch"
    );
    assert_eq!(
        restored
            .execute(
                NS,
                "validate_context",
                json!({"manifest":context["manifest"]})
            )
            .unwrap_err()
            .code,
        "commit_history_mismatch"
    );
    let replay = invoke(&mut restored, "candidate_submit", original_request);
    assert_eq!(replay["candidate_id"], original["candidate_id"]);
    assert_eq!(replay["state"], "withdrawn");
    assert!(!replay.to_string().contains("SENSITIVE"));
    for candidate in [&original, &waiting] {
        let current = invoke(
            &mut restored,
            "candidate_inspect",
            json!({"candidate_id":candidate["candidate_id"]}),
        );
        assert_eq!(current["state"], "withdrawn");
        assert_eq!(current["body_available"], false);
        assert!(current.get("body").is_none());
        assert!(!current.to_string().contains("SENSITIVE"));
    }
    assert_eq!(
        restored
            .execute(
                NS,
                "candidate_submit",
                submission("new-copy", "SENSITIVE_SYNTHETIC_NEW_COPY", fact("default")),
            )
            .unwrap_err()
            .code,
        "fact_retracted"
    );
    assert!(all_context(&mut restored)["items"]
        .as_array()
        .unwrap()
        .is_empty());
    let directory = temp.path().join("restored-view");
    restored.project(NS, &directory).unwrap();
    assert!(!read_projection(directory).unwrap()["body"]
        .as_str()
        .unwrap()
        .contains("SENSITIVE"));
}

#[test]
fn restore_requires_new_worker_lease_even_with_original_claim_idempotency_key() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("live.db")).unwrap();
    let initial = submit(&mut store, "source", "synthetic current source");
    let record = accept(&mut store, &initial, "accept-source");
    let summary = invoke(
        &mut store,
        "summary_create",
        json!({"inputs":[record["record_id"]],"processor":"synthetic",
               "idempotency_key":"summary"}),
    );
    let claim_request =
        json!({"worker":"worker","lease_seconds":60,"idempotency_key":"same-claim-key"});
    let old_lease = invoke(&mut store, "maintenance_claim", claim_request.clone());
    let snapshot = temp.path().join("snapshot.db");
    store.backup(&snapshot).unwrap();
    let destination = temp.path().join("restored.db");
    Store::restore(snapshot, &destination).unwrap();
    let mut restored = Store::open(destination).unwrap();
    // Receipt reset is important: replaying the original claim must not recover
    // the old live lease. A new claim is necessary in the new storage history.
    let new_lease = invoke(&mut restored, "maintenance_claim", claim_request);
    assert_eq!(new_lease["task_id"], summary["task_id"]);
    assert_ne!(new_lease["lease_token"], old_lease["lease_token"]);
    assert_ne!(new_lease["commit_token"], old_lease["commit_token"]);
    let late_request = json!({
        "task_id":summary["task_id"],"lease_token":old_lease["lease_token"],
        "body":"LATE_SYNTHETIC_BODY","idempotency_key":"completion"
    });
    assert_eq!(
        restored
            .execute(NS, "maintenance_complete", late_request)
            .unwrap_err()
            .code,
        "lease_conflict"
    );
    invoke(
        &mut restored,
        "maintenance_complete",
        json!({"task_id":summary["task_id"],"lease_token":new_lease["lease_token"],
               "body":"NEW_SYNTHETIC_BODY","idempotency_key":"completion"}),
    );
    let body = invoke(&mut restored, "inspect", json!({"id":summary["output_id"]}));
    assert_eq!(body["body"], "NEW_SYNTHETIC_BODY");
    let feed = invoke(&mut restored, "changes", json!({}));
    assert!(!feed.to_string().contains("SYNTHETIC_BODY"));
    assert_eq!(feed["events"].as_array().unwrap().len(), 2);
}

#[test]
fn actual_time_expiry_invalidates_context_without_an_outbox_event_or_commit_change() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("live.db")).unwrap();
    let expires = timestamp() + 3;
    invoke(
        &mut store,
        "remember",
        json!({"body":"TEMPORARY_SYNTHETIC_FACT","valid_until":expires,
               "idempotency_key":"temporary"}),
    );
    let context = all_context(&mut store);
    assert_eq!(context["items"].as_array().unwrap().len(), 1);
    let directory = temp.path().join("view");
    store.project(NS, &directory).unwrap();
    let before = read_projection(&directory).unwrap();
    assert_eq!(
        store.read_current_projection(NS, &directory).unwrap()["verification"],
        "sha256_and_live_store"
    );
    while timestamp() < expires {
        std::thread::sleep(Duration::from_millis(25));
    }
    let feed = invoke(
        &mut store,
        "changes",
        json!({"after":context["commit_token"]}),
    );
    assert_eq!(feed["events"], json!([]));
    assert_eq!(feed["expiry_requires_fresh_read"], true);
    let checked = invoke(
        &mut store,
        "validate_context",
        json!({"manifest":context["manifest"]}),
    );
    assert_eq!(checked["valid"], false);
    assert_eq!(checked["commit_token"], context["commit_token"]);
    assert_eq!(checked["invalid"][0]["reason"], "ineligible");
    // The standalone reader promises integrity only. Unchanged checksums do
    // not grant freshness or implicitly consult the live store.
    let stale_view = read_projection(&directory).unwrap();
    assert_eq!(stale_view["body"], before["body"]);
    assert_eq!(stale_view["verification"], "sha256_integrity_only");
    assert_eq!(stale_view["source_freshness"], "point_in_time_snapshot");
    assert_eq!(
        store
            .read_current_projection(NS, &directory)
            .unwrap_err()
            .code,
        "projection_stale"
    );
    store.project(NS, &directory).unwrap();
    let fresh_view = store.read_current_projection(NS, directory).unwrap();
    assert_eq!(
        fresh_view["manifest"]["commit_token"],
        context["commit_token"]
    );
    assert_eq!(fresh_view["manifest"]["records"], json!([]));
    assert!(!fresh_view["body"]
        .as_str()
        .unwrap()
        .contains("TEMPORARY_SYNTHETIC_FACT"));
}

#[test]
fn cli_requires_current_store_even_when_old_markdown_is_still_checksum_valid() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("live.db");
    let directory = temp.path().join("view");
    let mut store = Store::open(&database).unwrap();
    let candidate = submit(&mut store, "source", "STALE_CLI_SYNTHETIC_SOURCE");
    let record = accept(&mut store, &candidate, "accept-source");
    store.project(NS, &directory).unwrap();
    let snapshot = temp.path().join("snapshot.db");
    store.backup(&snapshot).unwrap();

    invoke(
        &mut store,
        "retract",
        json!({"id":record["record_id"],"expected_revision":1,"idempotency_key":"withdraw"}),
    );
    // Integrity alone still succeeds; the ordinary CLI must not return this body.
    assert!(read_projection(&directory).unwrap()["body"]
        .as_str()
        .unwrap()
        .contains("STALE_CLI_SYNTHETIC_SOURCE"));
    let read = |database: &std::path::Path, offline: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_recall-ledger"));
        command
            .arg("--db")
            .arg(database)
            .args(["--namespace", NS, "read-projection", "--directory"])
            .arg(&directory);
        if offline {
            command.arg("--offline-integrity-only");
        }
        command.output().unwrap()
    };
    let current = read(&database, false);
    assert!(!current.status.success());
    assert!(current.stdout.is_empty());
    let error: Value = serde_json::from_slice(&current.stderr).unwrap();
    assert_eq!(error["error"]["code"], "projection_stale");

    // The audit escape hatch is explicit and returns its weaker guarantee.
    let offline = read(&database, true);
    assert!(offline.status.success());
    let audit: Value = serde_json::from_slice(&offline.stdout).unwrap();
    assert_eq!(audit["verification"], "sha256_integrity_only");

    // An old backup contains identical bytes/revision, but its restored history
    // must not authorize the pre-restore projection.
    let restored = temp.path().join("restored.db");
    Store::restore(&snapshot, &restored).unwrap();
    let wrong_history = read(&restored, false);
    assert!(!wrong_history.status.success());
    let error: Value = serde_json::from_slice(&wrong_history.stderr).unwrap();
    assert_eq!(error["error"]["code"], "commit_history_mismatch");
    assert!(wrong_history.stdout.is_empty());
    assert_eq!(
        store
            .read_current_projection("wrong-namespace", &directory)
            .unwrap_err()
            .code,
        "namespace_mismatch"
    );

    store.project(NS, &directory).unwrap();
    let refreshed = read(&database, false);
    assert!(refreshed.status.success());
    let body: Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    assert_eq!(body["manifest"]["records"], json!([]));
    assert!(!body["body"]
        .as_str()
        .unwrap()
        .contains("STALE_CLI_SYNTHETIC_SOURCE"));
}

#[test]
fn edited_projection_blocks_regeneration_even_after_source_withdrawal() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("live.db")).unwrap();
    let candidate = submit(&mut store, "source", "WITHDRAWN_SYNTHETIC_SOURCE");
    let record = accept(&mut store, &candidate, "accept-source");
    let directory = temp.path().join("view");
    store.project(NS, &directory).unwrap();
    let pointer_before = fs::read(directory.join("CURRENT")).unwrap();
    let pointer: Value = serde_json::from_slice(&pointer_before).unwrap();
    let md_path = directory
        .join(pointer["generation"].as_str().unwrap())
        .join("MEMORY.md");
    let edited = "HUMAN_EDIT_OF_GENERATED_DATA";
    fs::write(&md_path, edited).unwrap();
    invoke(
        &mut store,
        "retract",
        json!({"id":record["record_id"],"expected_revision":1,"idempotency_key":"withdraw"}),
    );
    assert_eq!(
        store.project(NS, &directory).unwrap_err().code,
        "projection_modified"
    );
    assert_eq!(fs::read(directory.join("CURRENT")).unwrap(), pointer_before);
    assert_eq!(fs::read_to_string(md_path).unwrap(), edited);
    assert_eq!(
        read_projection(&directory).unwrap_err().code,
        "projection_modified"
    );
    assert_eq!(all_context(&mut store)["items"], json!([]));
}

#[test]
fn event_fingerprint_conflict_cannot_mutate_another_condition_or_emit_an_event() {
    let mut store = Store::open(":memory:").unwrap();
    let first_request = submission("event-1", "production in April", fact("production"));
    let first = invoke(&mut store, "candidate_submit", first_request.clone());
    let prod = accept(&mut store, &first, "accept-production");
    let stage_candidate = invoke(
        &mut store,
        "candidate_submit",
        submission("event-2", "staging in March", fact("staging")),
    );
    let stage = accept(&mut store, &stage_candidate, "accept-staging");
    assert_ne!(stage["record_id"], prod["record_id"]);
    let before = invoke(&mut store, "status", json!({}))["commit_token"].clone();
    let mut wrong_event = first_request;
    wrong_event["fact_key"] = fact("staging");
    wrong_event["idempotency_key"] = json!("different-operation-key");
    assert_eq!(
        store
            .execute(NS, "candidate_submit", wrong_event)
            .unwrap_err()
            .code,
        "event_conflict"
    );
    assert_eq!(
        invoke(&mut store, "changes", json!({"after":before}))["events"],
        json!([])
    );
    assert_eq!(
        invoke(
            &mut store,
            "fact_get",
            json!({"fact_key":fact("production")})
        )["body"],
        "production in April"
    );
    assert_eq!(
        invoke(&mut store, "fact_get", json!({"fact_key":fact("staging")}))["body"],
        "staging in March"
    );
    let manifest = all_context(&mut store)["manifest"].clone();
    assert_eq!(
        store
            .execute(
                "another-namespace",
                "validate_context",
                json!({"manifest":manifest})
            )
            .unwrap_err()
            .code,
        "namespace_mismatch"
    );
}
