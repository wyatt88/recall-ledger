//! Synthetic, local contract tests. No real user database or model is accessed.
use recall_ledger::Store;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn call(store: &mut Store, method: &str, args: Value) -> Value {
    store.execute("a", method, args).unwrap()
}
fn remember(store: &mut Store, key: &str, body: &str) -> Value {
    call(
        store,
        "remember",
        json!({"body":body,"idempotency_key":key}),
    )
}
fn derived(store: &mut Store, id: &Value, key: &str, body: &str) -> Value {
    let job = call(
        store,
        "prepare",
        json!({"inputs":[id],"processor":"fixture","idempotency_key":format!("p-{key}")}),
    );
    call(
        store,
        "publish",
        json!({"job_id":job["job_id"],"body":body,"idempotency_key":key}),
    )
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[test]
fn preview_explains_original_sources_transitive_impact_and_cas_without_writing() {
    let mut store = Store::open(":memory:").unwrap();
    let source = remember(&mut store, "s", "Original: only if the staging tests pass.");
    let summary = derived(&mut store, &source["id"], "d", "Conditional summary.");
    let nested = derived(&mut store, &summary["id"], "n", "Second generation.");
    call(
        &mut store,
        "prepare",
        json!({"inputs":[nested["id"]],"processor":"fixture","idempotency_key":"pending"}),
    );
    let before = call(&mut store, "status", json!({}))["commit_token"].clone();
    let impact = call(
        &mut store,
        "impact_preview",
        json!({"id":source["id"],"expected_revision":1}),
    );
    assert_eq!(
        impact["sources"][0]["body"],
        "Original: only if the staging tests pass."
    );
    assert_eq!(impact["descendants"].as_array().unwrap().len(), 2);
    assert_eq!(impact["effects"]["revise"]["invalidated_descendants"], 2);
    assert_eq!(impact["effects"]["prepared_jobs_affected"], 1);
    assert_eq!(impact["preview_only"], true);
    assert_eq!(before, impact["commit_token"]);
    assert_eq!(
        call(&mut store, "changes", json!({"after":before}))["events"],
        json!([])
    );
    assert_eq!(
        store
            .execute(
                "a",
                "impact_preview",
                json!({"id":source["id"],"expected_revision":2})
            )
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    assert_eq!(
        store
            .execute("b", "impact_preview", json!({"id":source["id"]}))
            .unwrap_err()
            .code,
        "not_found"
    );
    let from_summary = call(&mut store, "impact_preview", json!({"id":summary["id"]}));
    assert_eq!(from_summary["sources"][0]["id"], source["id"]);
    assert_eq!(from_summary["dependencies"][0]["captured_revision"], 1);
}

#[test]
fn history_keeps_corrections_auditable_but_never_promotes_old_facts() {
    let mut store = Store::open(":memory:").unwrap();
    let source = call(
        &mut store,
        "remember",
        json!({
            "body":"OLD launch in April","occurred_at":100,"idempotency_key":"s"
        }),
    );
    let args = json!({"id":source["id"],"expected_revision":1,"body":"NEW launch in August","idempotency_key":"r"});
    let revision = call(&mut store, "revise", args.clone());
    assert_eq!(call(&mut store, "revise", args), revision);
    let history = call(&mut store, "timeline", json!({}));
    assert_eq!(history["events"].as_array().unwrap().len(), 2);
    assert_eq!(history["events"][0]["revision"], 2);
    assert_eq!(history["events"][0]["occurred_at"], 100);
    assert_eq!(history["events"][0]["usable_as_current"], true);
    assert_eq!(history["events"][1]["body"], "OLD launch in April");
    assert_eq!(history["events"][1]["historical_only"], true);
    assert_eq!(history["events"][1]["usable_as_current"], false);
    let snapshot = call(&mut store, "timeline", json!({"as_of":now()}));
    assert_eq!(snapshot["events"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["events"][0]["revision"], 2);
    assert!(!snapshot.to_string().contains("OLD launch"));
    call(
        &mut store,
        "revise",
        json!({
            "id":source["id"],"expected_revision":2,"body":"NEW confirmed August",
            "occurred_at":null,"idempotency_key":"r-clear"
        }),
    );
    assert!(call(&mut store, "timeline", json!({}))["events"][0]["occurred_at"].is_null());
}

#[test]
fn repaired_summary_does_not_restore_a_historical_invalidated_body() {
    let mut store = Store::open(":memory:").unwrap();
    let source = remember(&mut store, "source", "SOURCE_OLD");
    let task = call(
        &mut store,
        "summary_create",
        json!({
            "inputs":[source["id"]],"processor":"fixture","max_chars":1000,"idempotency_key":"task"
        }),
    );
    let lease = call(
        &mut store,
        "maintenance_claim",
        json!({
            "worker":"fixture","lease_seconds":60,"idempotency_key":"lease-1"
        }),
    );
    let summary = call(
        &mut store,
        "maintenance_complete",
        json!({
            "task_id":task["task_id"],"lease_token":lease["lease_token"],"body":"SUMMARY_OLD","idempotency_key":"complete-1"
        }),
    );
    call(
        &mut store,
        "revise",
        json!({
            "id":source["id"],"expected_revision":1,"body":"SOURCE_NEW","idempotency_key":"correct"
        }),
    );
    let stale = call(&mut store, "impact_preview", json!({"id":summary["id"]}));
    assert!(stale["record"].get("body").is_none());
    assert!(!stale.to_string().contains("SUMMARY_OLD"));
    assert_eq!(stale["sources"][0]["body"], "SOURCE_NEW");
    let lease = call(
        &mut store,
        "maintenance_claim",
        json!({
            "worker":"fixture","lease_seconds":60,"idempotency_key":"lease-2"
        }),
    );
    assert_eq!(lease["claimed"], true);
    call(
        &mut store,
        "maintenance_complete",
        json!({
            "task_id":task["task_id"],"lease_token":lease["lease_token"],"body":"SUMMARY_NEW","idempotency_key":"complete-2"
        }),
    );
    let history = call(&mut store, "timeline", json!({"query":summary["id"]}));
    assert!(!history.to_string().contains("SUMMARY_OLD"));
    assert!(history.to_string().contains("SUMMARY_NEW"));
    let old = history["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["revision"] == 1 && event["type"] == "revision")
        .unwrap();
    assert_eq!(old["body_withheld_reason"], "ancestor_changed_or_withdrawn");
    assert_eq!(
        call(&mut store, "timeline", json!({"query":"SUMMARY_OLD"}))["events"],
        json!([])
    );
}

#[test]
fn withdrawal_hides_all_versions_sources_and_search_hits_even_before_cutoff() {
    let mut store = Store::open(":memory:").unwrap();
    let source = remember(&mut store, "s", "SECRET_OLD");
    derived(&mut store, &source["id"], "d", "SECRET_DERIVED");
    call(
        &mut store,
        "revise",
        json!({
            "id":source["id"],"expected_revision":1,"body":"SECRET_NEW","idempotency_key":"r"
        }),
    );
    let cutoff = now();
    call(
        &mut store,
        "retract",
        json!({
            "id":source["id"],"expected_revision":2,"idempotency_key":"forget"
        }),
    );
    for args in [
        json!({}),
        json!({"as_of":cutoff}),
        json!({"query":"SECRET"}),
    ] {
        let result = call(&mut store, "timeline", args);
        assert!(!result.to_string().contains("SECRET"));
        assert!(result["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e.get("body").is_none()));
    }
    assert_eq!(
        call(&mut store, "timeline", json!({"query":"SECRET"}))["events"],
        json!([])
    );
    assert_eq!(
        store
            .execute("a", "impact_preview", json!({"id":source["id"]}))
            .unwrap_err()
            .code,
        "not_found"
    );
}

#[test]
fn cursor_is_bounded_query_scoped_and_rejects_writes_or_restore_between_pages() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("live")).unwrap();
    for key in ["one", "two", "three"] {
        remember(&mut store, key, key);
    }
    let first = call(&mut store, "timeline", json!({"limit":1}));
    assert_eq!(first["has_more"], true);
    let cursor = first["next_cursor"].clone();
    let second = call(&mut store, "timeline", json!({"limit":1,"cursor":cursor}));
    assert_ne!(first["events"][0]["id"], second["events"][0]["id"]);
    assert_eq!(
        store
            .execute("a", "timeline", json!({"query":"changed","cursor":cursor}))
            .unwrap_err()
            .code,
        "invalid_request"
    );
    assert_eq!(
        store
            .execute("b", "timeline", json!({"cursor":cursor}))
            .unwrap_err()
            .code,
        "invalid_request"
    );
    let backup = temp.path().join("backup");
    store.backup(&backup).unwrap();
    let restore = temp.path().join("restored");
    Store::restore(&backup, &restore).unwrap();
    let mut restored = Store::open(restore).unwrap();
    assert_eq!(
        restored
            .execute("a", "timeline", json!({"cursor":cursor}))
            .unwrap_err()
            .code,
        "stale_cursor"
    );
    remember(&mut store, "four", "four");
    assert_eq!(
        store
            .execute("a", "timeline", json!({"cursor":cursor}))
            .unwrap_err()
            .code,
        "stale_cursor"
    );
}

#[test]
fn cursor_size_does_not_expand_with_escaped_query_text() {
    let mut store = Store::open(":memory:").unwrap();
    let query = format!("{}x", "\n".repeat(3000));
    remember(&mut store, "one", &query);
    remember(&mut store, "two", &query);
    let first = call(&mut store, "timeline", json!({"query":query,"limit":1}));
    assert!(first["next_cursor"].as_str().unwrap().len() < 512);
    let second = call(
        &mut store,
        "timeline",
        json!({"query":query,"cursor":first["next_cursor"],"limit":1}),
    );
    assert_eq!(second["events"].as_array().unwrap().len(), 1);
    assert_eq!(second["has_more"], false);
}

#[test]
fn occurrence_is_not_knowledge_time_and_expired_sources_are_historical_only() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let at = now();
    let record = call(
        &mut store,
        "remember",
        json!({
            "body":"historical observed event","occurred_at":10,"valid_from":20,
            "valid_until":at-5,"idempotency_key":"past"
        }),
    );
    // The old occurrence date does not backdate this new admission.
    assert_eq!(
        call(&mut store, "timeline", json!({"as_of":at-1}))["events"],
        json!([])
    );
    drop(store);
    // A synthetic recorded-at fixture tests a past checkpoint without waiting.
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE history_events SET recorded_at=?1 WHERE record_id=?2",
        params![at - 10, record["id"].as_str().unwrap()],
    )
    .unwrap();
    drop(db);
    let mut store = Store::open(&path).unwrap();
    let past = call(&mut store, "timeline", json!({"as_of":at-8}));
    assert_eq!(past["events"][0]["body"], "historical observed event");
    assert_eq!(past["events"][0]["occurred_at"], 10);
    assert_eq!(past["events"][0]["usable_as_current"], false);
    assert_eq!(past["events"][0]["historical_only"], true);
    let later = call(&mut store, "timeline", json!({"as_of":at-2}));
    assert_eq!(
        later["events"][0]["body_withheld_reason"],
        "outside_validity_at_time"
    );
    assert!(later["events"][0].get("body").is_none());
    assert_eq!(
        store
            .execute("a", "timeline", json!({"as_of":at+10000}))
            .unwrap_err()
            .code,
        "invalid_request"
    );
}

#[test]
fn schema_four_migration_preserves_identity_receipts_and_unknown_history_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let first = remember(&mut store, "s", "OLD legacy");
    call(
        &mut store,
        "revise",
        json!({
            "id":first["id"],"expected_revision":1,"body":"CURRENT legacy","idempotency_key":"r"
        }),
    );
    let binding = call(&mut store, "binding", json!({}));
    drop(store);
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "DROP TRIGGER history_revision_insert;DROP TRIGGER history_status_update;
        DROP TABLE history_events;PRAGMA user_version=4;",
    )
    .unwrap();
    drop(db);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(call(&mut store, "status", json!({}))["schema_version"], 5);
    assert_eq!(call(&mut store, "binding", json!({})), binding);
    assert_eq!(
        call(
            &mut store,
            "operation",
            json!({"method":"remember","idempotency_key":"s"})
        ),
        first
    );
    let history = call(&mut store, "timeline", json!({}));
    assert_eq!(history["events"].as_array().unwrap().len(), 2);
    assert_eq!(history["events"][0]["body"], "CURRENT legacy");
    assert!(history["events"][0]["occurred_at"].is_null());
    assert!(history["events"][0]["commit_sequence"].is_null());
    assert_eq!(
        history["events"][1]["body_withheld_reason"],
        "historical_dependencies_unknown"
    );
    let backup = temp.path().join("backup");
    store.backup(&backup).unwrap();
    Store::restore(&backup, temp.path().join("restore")).unwrap();
}

#[test]
fn an_earlier_cutoff_selects_one_revision_but_current_ancestor_corrections_still_win() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let source = remember(&mut store, "s", "PREVIOUS checkpoint");
    let summary = derived(&mut store, &source["id"], "d", "OLD dependent checkpoint");
    call(
        &mut store,
        "revise",
        json!({
            "id":source["id"],"expected_revision":1,"body":"CURRENT checkpoint","idempotency_key":"r"
        }),
    );
    drop(store);
    let db = Connection::open(&path).unwrap();
    // Synthetic event-clock fixture: source and summary at 100, correction at 200.
    db.execute(
        "UPDATE history_events SET recorded_at=CASE WHEN method='revise' THEN 200 ELSE 100 END",
        [],
    )
    .unwrap();
    drop(db);
    let mut store = Store::open(&path).unwrap();
    let prior = call(&mut store, "timeline", json!({"as_of":150}));
    let source_event = prior["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == source["id"])
        .unwrap();
    assert_eq!(source_event["revision"], 1);
    assert_eq!(source_event["body"], "PREVIOUS checkpoint");
    assert_eq!(source_event["usable_as_current"], false);
    let summary_event = prior["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == summary["id"])
        .unwrap();
    assert!(summary_event.get("body").is_none());
    assert_eq!(summary_event["body_withheld_reason"], "stale_dependency");
    let current = call(
        &mut store,
        "timeline",
        json!({"as_of":250,"query":source["id"]}),
    );
    assert_eq!(current["events"].as_array().unwrap().len(), 1);
    assert_eq!(current["events"][0]["revision"], 2);
}

#[test]
fn portable_import_records_local_admission_and_does_not_invent_occurrence_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source");
    let mut source = Store::open(&path).unwrap();
    let record = call(
        &mut source,
        "remember",
        json!({
            "body":"imported old event","occurred_at":50,"idempotency_key":"s"
        }),
    );
    drop(source);
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE records SET created_at=100,updated_at=100 WHERE id=?1",
        [record["id"].as_str().unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE revisions SET recorded_at=100 WHERE id=?1",
        [record["id"].as_str().unwrap()],
    )
    .unwrap();
    drop(db);
    let mut source = Store::open(&path).unwrap();
    let bundle = call(&mut source, "bundle_export", json!({}));
    let mut destination = Store::open(":memory:").unwrap();
    let plan = call(
        &mut destination,
        "bundle_plan",
        json!({"bundle":bundle,"expected_source":bundle["payload"]["source_binding"]}),
    );
    let receipt = call(
        &mut destination,
        "bundle_apply",
        json!({
            "bundle":bundle,"expected_source":bundle["payload"]["source_binding"],"expected_target_commit":plan["target_commit"],
            "reviewed_digest":plan["reviewed_digest"],"idempotency_key":"import"
        }),
    );
    let history = call(&mut destination, "timeline", json!({}));
    assert_eq!(history["events"].as_array().unwrap().len(), 1);
    assert_eq!(history["events"][0]["method"], "bundle_apply");
    assert!(history["events"][0]["recorded_at"].as_i64().unwrap() > 1000);
    assert!(history["events"][0]["occurred_at"].is_null());
    assert_eq!(
        call(&mut destination, "timeline", json!({"as_of":101}))["events"],
        json!([])
    );
    assert_eq!(
        call(&mut destination, "status", json!({}))["commit_token"],
        receipt["commit_token"]
    );
}

fn cli(path: &Path, method: &str, text: &str, expected: Option<&Value>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_recall-ledger"));
    command
        .arg("--db")
        .arg(path)
        .args(["--namespace", "a", "call", method, "--params-file", "-"]);
    if let Some(binding) = expected {
        command.arg("--expected-binding").arg(binding.to_string());
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn cli_accepts_large_stdin_and_pins_destination_before_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let result = cli(
        &path,
        "remember",
        &format!(
            "{}{}",
            " ".repeat(300_000),
            json!({"body":"stdin fact","idempotency_key":"s"})
        ),
        None,
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let record: Value = serde_json::from_slice(&result.stdout).unwrap();
    let mut store = Store::open(&path).unwrap();
    let binding = call(&mut store, "binding", json!({}));
    let result = cli(
        &path,
        "impact_preview",
        &json!({"id":record["id"]}).to_string(),
        Some(&binding),
    );
    assert!(result.status.success());
    let mut wrong = binding.clone();
    wrong["namespace"] = json!("b");
    let denied = cli(
        &path,
        "remember",
        &json!({"body":"must not persist","idempotency_key":"bad"}).to_string(),
        Some(&wrong),
    );
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("binding_mismatch"));
    assert_eq!(call(&mut store, "status", json!({}))["records"], 1);
    let conflict = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
        .arg("--db")
        .arg(&path)
        .args(["call", "timeline", "--params", "{}", "--params-file", "-"])
        .output()
        .unwrap();
    assert!(!conflict.status.success());
    let args = temp.path().join("params.json");
    std::fs::write(&args, b"{}").unwrap();
    let from_file = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
        .arg("--db")
        .arg(&path)
        .args(["--namespace", "a", "call", "timeline", "--params-file"])
        .arg(args)
        .output()
        .unwrap();
    assert!(from_file.status.success());
}

#[cfg(unix)]
#[test]
fn ipc_exposes_both_read_methods_with_atomic_binding_checks() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::Builder::new()
        .prefix("rl-history-")
        .tempdir_in("/tmp")
        .unwrap();
    let db = temp.path().join("db");
    let mut store = Store::open(&db).unwrap();
    let record = remember(&mut store, "s", "IPC source");
    let pinned = call(&mut store, "binding", json!({}));
    let socket = temp.path().join("s");
    let mut daemon = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
            .arg("--db")
            .arg(&db)
            .args(["--namespace", "a", "serve", "--socket"])
            .arg(&socket)
            .arg("--no-worker")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(&socket).is_err() {
        assert!(daemon.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    for (method, args) in [
        ("impact_preview", json!({"id":record["id"]})),
        ("timeline", json!({})),
    ] {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            json!({"id":1,"method":method,"params":args,"expected_binding":pinned})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert!(response.get("result").is_some(), "{response}");
        assert!(response.to_string().contains("IPC source"));
    }
}
