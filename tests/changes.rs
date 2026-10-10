use recall_ledger::Store;
use serde_json::{json, Value};

fn remember(store: &mut Store, ns: &str, key: &str, body: &str) -> Value {
    store
        .execute(ns, "remember", json!({"body":body,"idempotency_key":key}))
        .unwrap()
}

#[test]
fn feed_is_atomic_body_free_namespaced_and_retry_stable() {
    let mut store = Store::open(":memory:").unwrap();
    let first = remember(&mut store, "a", "one", "private alpha");
    assert_eq!(first, remember(&mut store, "a", "one", "private alpha"));
    remember(&mut store, "b", "other", "other namespace secret");
    let second = remember(&mut store, "a", "two", "private beta");
    let third = store
        .execute(
            "a",
            "retract",
            json!({"id":first["id"],"expected_revision":1,"idempotency_key":"withdraw"}),
        )
        .unwrap();
    let first_page = store.execute("a", "changes", json!({"limit":1})).unwrap();
    assert_eq!(first_page["events"].as_array().unwrap().len(), 1);
    assert_eq!(first_page["cursor"], first["commit_token"]);
    assert_eq!(first_page["has_more"], true);
    assert!(!first_page.to_string().contains("private"));
    let second_page = store
        .execute(
            "a",
            "changes",
            json!({"after":first_page["cursor"],"limit":1}),
        )
        .unwrap();
    assert_eq!(second_page["cursor"], second["commit_token"]);
    assert_eq!(second_page["events"][0]["method"], "remember");
    let last = store
        .execute(
            "a",
            "changes",
            json!({"after":second_page["cursor"],"limit":1}),
        )
        .unwrap();
    assert_eq!(last["cursor"], third["commit_token"]);
    assert_eq!(last["has_more"], false);
    assert_eq!(last["events"][0]["record_ids"], json!([first["id"]]));
    let empty = store
        .execute("a", "changes", json!({"after":last["cursor"]}))
        .unwrap();
    assert_eq!(empty["events"], json!([]));
    assert_eq!(empty["resync_required"], false);
}

#[test]
fn failed_mutations_leave_no_event_and_restart_keeps_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let receipt = remember(&mut store, "a", "one", "original");
    let err = store
        .execute(
            "a",
            "revise",
            json!({"id":receipt["id"],"expected_revision":99,"body":"wrong","idempotency_key":"bad"}),
        )
        .unwrap_err();
    assert_eq!(err.code, "revision_conflict");
    drop(store);
    let mut store = Store::open(path).unwrap();
    let result = store
        .execute("a", "changes", json!({"after":receipt["commit_token"]}))
        .unwrap();
    assert_eq!(result["events"], json!([]));
    assert_eq!(result["cursor"], receipt["commit_token"]);
    assert_eq!(
        store.execute("a", "changes", json!({})).unwrap()["events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn context_revalidates_revisions_dependencies_and_retraction() {
    let mut store = Store::open(":memory:").unwrap();
    let source = remember(&mut store, "a", "one", "April release");
    let job = store
        .execute(
            "a",
            "prepare",
            json!({"inputs":[source["id"]],"processor":"fixture","idempotency_key":"p"}),
        )
        .unwrap();
    store
        .execute(
            "a",
            "publish",
            json!({"job_id":job["job_id"],"body":"April summary","idempotency_key":"s"}),
        )
        .unwrap();
    let recall = store.execute("a", "recall", json!({})).unwrap();
    assert_eq!(recall["manifest"]["records"].as_array().unwrap().len(), 2);
    assert_eq!(
        store
            .execute(
                "a",
                "validate_context",
                json!({"manifest":recall["manifest"]})
            )
            .unwrap()["valid"],
        true
    );
    store.execute("a", "revise", json!({"id":source["id"],"expected_revision":1,"body":"August release","idempotency_key":"r"})).unwrap();
    let invalid = store
        .execute(
            "a",
            "validate_context",
            json!({"manifest":recall["manifest"]}),
        )
        .unwrap();
    assert_eq!(invalid["valid"], false);
    assert_eq!(invalid["invalid"].as_array().unwrap().len(), 2);
    assert!(!invalid.to_string().contains("April"));
    let fresh = store.execute("a", "recall", json!({})).unwrap();
    assert_eq!(fresh["manifest"]["records"].as_array().unwrap().len(), 1);
    store
        .execute(
            "a",
            "retract",
            json!({"id":source["id"],"expected_revision":2,"idempotency_key":"x"}),
        )
        .unwrap();
    assert_eq!(
        store
            .execute(
                "a",
                "validate_context",
                json!({"manifest":fresh["manifest"]})
            )
            .unwrap()["valid"],
        false
    );
}

#[test]
fn rejects_foreign_future_and_malformed_cursors_and_manifests() {
    let mut store = Store::open(":memory:").unwrap();
    let mut other = Store::open(":memory:").unwrap();
    let token = other.execute("a", "status", json!({})).unwrap()["commit_token"].clone();
    assert_eq!(
        store
            .execute("a", "changes", json!({"after":token}))
            .unwrap_err()
            .code,
        "commit_history_mismatch"
    );
    let current = store.execute("a", "status", json!({})).unwrap()["commit_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let history = current.rsplit_once(':').unwrap().0;
    assert_eq!(
        store
            .execute("a", "changes", json!({"after":format!("{history}:999")}))
            .unwrap_err()
            .code,
        "required_consistency_unavailable"
    );
    for params in [
        json!({"after":12}),
        json!({"after":"bad"}),
        json!({"limit":0}),
        json!({"limit":501}),
    ] {
        assert!(store.execute("a", "changes", params).is_err());
    }
    let manifest = store.execute("a", "recall", json!({})).unwrap()["manifest"].clone();
    assert_eq!(
        store
            .execute("b", "validate_context", json!({"manifest":manifest}))
            .unwrap_err()
            .code,
        "namespace_mismatch"
    );
    let mut bad = manifest.clone();
    bad["records"] = json!([{"id":"none","revision":0}]);
    assert!(store
        .execute("a", "validate_context", json!({"manifest":bad}))
        .is_err());
    let mut missing = manifest;
    missing.as_object_mut().unwrap().remove("commit_token");
    assert!(store
        .execute("a", "validate_context", json!({"manifest":missing}))
        .is_err());
}

#[test]
fn expiry_invalidates_context_without_a_database_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let record = remember(&mut store, "a", "one", "temporary condition");
    let manifest = store.execute("a", "recall", json!({})).unwrap()["manifest"].clone();
    // Simulate wall-clock expiry without sleeping or changing the commit sequence.
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute(
        "UPDATE records SET valid_until=1 WHERE id=?1",
        [record["id"].as_str().unwrap()],
    )
    .unwrap();
    let result = store
        .execute("a", "validate_context", json!({"manifest":manifest}))
        .unwrap();
    assert_eq!(result["valid"], false);
    assert_eq!(result["commit_token"], manifest["commit_token"]);
    assert_eq!(result["invalid"][0]["reason"], "ineligible");
}
