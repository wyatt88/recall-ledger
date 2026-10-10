use recall_ledger::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;

fn remember(store: &mut Store, body: &str, key: &str) -> Value {
    store
        .execute(
            "test",
            "remember",
            json!({"body":body,"idempotency_key":key}),
        )
        .unwrap()
}

#[test]
fn active_wal_backup_restores_snapshot_with_new_identity_and_no_old_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("live.db");
    let backup = temp.path().join("snapshot.db");
    let restored = temp.path().join("restored.db");
    let mut store = Store::open(&database).unwrap();
    let original = remember(&mut store, "state before backup", "original");
    assert!(
        fs::metadata(database.with_extension("db-wal"))
            .unwrap()
            .len()
            > 0
    );
    let receipt = store.backup(&backup).unwrap();
    assert_eq!(receipt["commit_token"], original["commit_token"]);
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&backup).unwrap())),
        receipt["sha256"]
    );
    let original_backup = fs::read(&backup).unwrap();
    store
        .execute(
            "test",
            "revise",
            json!({"id":original["id"],"expected_revision":1,"body":"state after backup",
                   "idempotency_key":"later"}),
        )
        .unwrap();
    let recovery = Store::restore(&backup, &restored).unwrap();
    assert_eq!(original_backup, fs::read(&backup).unwrap());
    assert_ne!(recovery["commit_token"], original["commit_token"]);
    assert!(recovery["commit_token"].as_str().unwrap().ends_with(":0"));
    let mut recovered = Store::open(&restored).unwrap();
    let record = recovered
        .execute("test", "inspect", json!({"id":original["id"]}))
        .unwrap();
    assert_eq!(record["body"], "state before backup");
    assert_eq!(record["revision"], 1);
    assert_eq!(
        recovered
            .execute(
                "test",
                "recall",
                json!({"minimum_commit":original["commit_token"]})
            )
            .unwrap_err()
            .code,
        "commit_history_mismatch"
    );
    assert_eq!(
        recovered
            .execute("test", "changes", json!({"after":original["commit_token"]}))
            .unwrap_err()
            .code,
        "commit_history_mismatch"
    );
    let initial_changes = recovered.execute("test", "changes", json!({})).unwrap();
    assert!(initial_changes["events"].as_array().unwrap().is_empty());
    assert_eq!(
        recovered
            .execute(
                "test",
                "operation",
                json!({"method":"remember","idempotency_key":"original"})
            )
            .unwrap_err()
            .code,
        "not_found"
    );
    // A new write gets a receipt from the new history, never an old token.
    let fresh = remember(&mut recovered, "new history record", "fresh");
    assert_eq!(
        fresh["commit_token"].as_str().unwrap().split(':').next(),
        recovery["commit_token"].as_str().unwrap().split(':').next()
    );
    let changes: i64 = Connection::open(&restored)
        .unwrap()
        .query_row("SELECT count(*) FROM change_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(changes, 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&restored).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn recovery_cancels_prepared_work_and_resets_old_worker_leases() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("live.db")).unwrap();
    let record = remember(&mut store, "synthetic input", "seed");
    let prepared = store
        .execute(
            "test",
            "prepare",
            json!({"inputs":[record["id"]],"processor":"fixture",
                   "idempotency_key":"prepared"}),
        )
        .unwrap();
    let summary = store
        .execute(
            "test",
            "summary_create",
            json!({"inputs":[record["id"]],"processor":"fixture","max_chars":100,
                   "idempotency_key":"managed"}),
        )
        .unwrap();
    let lease = store
        .execute(
            "test",
            "maintenance_claim",
            json!({"worker":"original-worker","lease_seconds":60,"idempotency_key":"claim"}),
        )
        .unwrap();
    assert_eq!(lease["claimed"], true);
    let snapshot = temp.path().join("snapshot.db");
    store.backup(&snapshot).unwrap();
    let destination = temp.path().join("recovered.db");
    let receipt = Store::restore(&snapshot, &destination).unwrap();
    assert_eq!(receipt["reset_leases"], 1);
    assert_eq!(receipt["invalidated_prepared_jobs"], 1);
    let mut recovered = Store::open(destination).unwrap();
    assert_eq!(
        recovered
            .execute(
                "test",
                "publish",
                json!({"job_id":prepared["job_id"],"body":"late body",
                       "idempotency_key":"late-publish"})
            )
            .unwrap_err()
            .code,
        "stale_dependency"
    );
    assert_eq!(
        recovered
            .execute(
                "test",
                "maintenance_complete",
                json!({"task_id":summary["task_id"],"lease_token":lease["lease_token"],
                       "body":"late managed body","idempotency_key":"late-complete"})
            )
            .unwrap_err()
            .code,
        "lease_conflict"
    );
    let new_lease = recovered
        .execute(
            "test",
            "maintenance_claim",
            json!({"worker":"new-worker","lease_seconds":60,"idempotency_key":"new-claim"}),
        )
        .unwrap();
    assert_eq!(new_lease["claimed"], true);
    assert_ne!(new_lease["lease_token"], lease["lease_token"]);
    assert!(new_lease["generation"].as_i64().unwrap() > lease["generation"].as_i64().unwrap());
}

#[test]
fn rejects_overwrite_unrelated_or_old_schema_without_changing_existing_files() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("live.db")).unwrap();
    let version = store.execute("test", "status", json!({})).unwrap()["schema_version"]
        .as_i64()
        .unwrap();
    let existing = temp.path().join("keep.txt");
    fs::write(&existing, b"keep this file").unwrap();
    assert_eq!(
        store.backup(&existing).unwrap_err().code,
        "destination_exists"
    );
    let snapshot = temp.path().join("snapshot.db");
    store.backup(&snapshot).unwrap();
    assert_eq!(
        Store::restore(&snapshot, &existing).unwrap_err().code,
        "destination_exists"
    );
    assert_eq!(fs::read(&existing).unwrap(), b"keep this file");
    let foreign = temp.path().join("foreign.db");
    let db = Connection::open(&foreign).unwrap();
    db.execute_batch("CREATE TABLE unrelated(data TEXT);")
        .unwrap();
    db.pragma_update(None, "user_version", version).unwrap();
    drop(db);
    let destination = temp.path().join("should-not-exist.db");
    assert_eq!(
        Store::restore(&foreign, &destination).unwrap_err().code,
        "schema_mismatch"
    );
    assert!(!destination.exists());
    let db = Connection::open(&snapshot).unwrap();
    db.pragma_update(None, "user_version", version - 1).unwrap();
    drop(db);
    assert_eq!(
        Store::restore(&snapshot, &destination).unwrap_err().code,
        "schema_mismatch"
    );
    assert!(!destination.exists());
}

#[test]
fn refuses_existing_sqlite_sidecars_even_when_the_main_destination_is_absent() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("live.db")).unwrap();
    let snapshot = temp.path().join("snapshot.db");
    store.backup(&snapshot).unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        let destination = temp.path().join(format!("target{suffix}.db"));
        let mut sibling = destination.as_os_str().to_os_string();
        sibling.push(suffix);
        let sibling = std::path::PathBuf::from(sibling);
        fs::write(&sibling, b"unknown existing SQLite state").unwrap();
        assert!(!destination.exists());
        assert_eq!(
            Store::restore(&snapshot, &destination).unwrap_err().code,
            "destination_exists"
        );
        assert_eq!(
            store.backup(&destination).unwrap_err().code,
            "destination_exists"
        );
        assert_eq!(fs::read(sibling).unwrap(), b"unknown existing SQLite state");
        assert!(!destination.exists());
    }
}

#[test]
fn rejects_modified_foreign_key_unique_and_index_definitions() {
    let changes = [
        ("edges", "FOREIGN KEY(child_id) REFERENCES records(id),", ""),
        ("fact_bindings", "UNIQUE(namespace,record_id),", ""),
        ("records_scope", "namespace,status", "namespace,kind"),
    ];
    for (object, before, after) in changes {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("live.db")).unwrap();
        let snapshot = temp.path().join("snapshot.db");
        store.backup(&snapshot).unwrap();
        let source = Connection::open(&snapshot).unwrap();
        let original: String = source
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name=?1",
                [object],
                |row| row.get(0),
            )
            .unwrap();
        let changed = original.replace(before, after);
        assert_ne!(original, changed);
        source.pragma_update(None, "writable_schema", true).unwrap();
        source
            .execute(
                "UPDATE sqlite_schema SET sql=?1 WHERE name=?2",
                rusqlite::params![changed, object],
            )
            .unwrap();
        drop(source);
        let original_bytes = fs::read(&snapshot).unwrap();
        let destination = temp.path().join("restored.db");
        let rejected = Store::restore(&snapshot, &destination).unwrap_err();
        // Removing a UNIQUE clause can make its remaining SQLite autoindex
        // malformed, so SQLite may reject it before our inventory comparison.
        assert!(
            matches!(rejected.code.as_str(), "schema_mismatch" | "database_error"),
            "modified {object} was not rejected as a schema error"
        );
        assert!(!destination.exists());
        assert_eq!(fs::read(&snapshot).unwrap(), original_bytes);
    }
}

#[test]
fn restore_preserves_candidate_deduplication_and_fact_binding_tables() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("live.db")).unwrap();
    let key =
        json!({"entity":"project","attribute":"language","scope":"example","conditions":"always"});
    let submission = json!({"body":"Use Rust","event_id":"event-1","origin":"user_explicit",
                           "source":"synthetic-test","fact_key":key,"idempotency_key":"capture-1"});
    let candidate = store
        .execute("test", "candidate_submit", submission.clone())
        .unwrap();
    let accepted = store.execute(
        "test", "candidate_resolve",
        json!({"candidate_id":candidate["candidate_id"],"decision":"accept","idempotency_key":"accept-1"}),
    ).unwrap();
    let pending = store
        .execute(
            "test",
            "candidate_submit",
            json!({"body":"Review this later","event_id":"event-2","origin":"model_inference",
               "source":"synthetic-test","idempotency_key":"pending-1"}),
        )
        .unwrap();
    let snapshot = temp.path().join("snapshot.db");
    store.backup(&snapshot).unwrap();
    let destination = temp.path().join("restored.db");
    let recovery = Store::restore(&snapshot, &destination).unwrap();
    let source = Connection::open(&snapshot).unwrap();
    let restored = Connection::open(&destination).unwrap();
    for name in ["ingestion_candidates", "fact_bindings"] {
        let query = format!("SELECT count(*) FROM {name}");
        let before: i64 = source.query_row(&query, [], |row| row.get(0)).unwrap();
        let after: i64 = restored.query_row(&query, [], |row| row.get(0)).unwrap();
        assert_eq!(before, after);
        assert!(after > 0);
    }
    drop(restored);
    let mut recovered = Store::open(&destination).unwrap();
    let duplicate = recovered
        .execute("test", "candidate_submit", submission)
        .unwrap();
    assert_eq!(duplicate["candidate_id"], candidate["candidate_id"]);
    assert_eq!(duplicate["state"], "accepted");
    assert_eq!(
        duplicate["commit_token"]
            .as_str()
            .unwrap()
            .split(':')
            .next(),
        recovery["commit_token"].as_str().unwrap().split(':').next()
    );
    let fact = recovered
        .execute("test", "fact_get", json!({"fact_key":key}))
        .unwrap();
    assert_eq!(fact["id"], accepted["record_id"]);
    assert_eq!(fact["body"], "Use Rust");
    let pending_after = recovered
        .execute(
            "test",
            "candidate_inspect",
            json!({"candidate_id":pending["candidate_id"]}),
        )
        .unwrap();
    assert_eq!(pending_after["state"], "pending");
    assert_eq!(pending_after["body"], "Review this later");
}

#[cfg(unix)]
#[test]
fn refuses_symlinks_and_parent_traversal() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("live.db")).unwrap();
    let snapshot = temp.path().join("snapshot.db");
    store.backup(&snapshot).unwrap();
    let link = temp.path().join("link.db");
    symlink(&snapshot, &link).unwrap();
    assert_eq!(store.backup(&link).unwrap_err().code, "invalid_path");
    assert_eq!(
        Store::restore(&link, temp.path().join("restored.db"))
            .unwrap_err()
            .code,
        "invalid_path"
    );
    let directory = temp.path().join("directory");
    fs::create_dir(&directory).unwrap();
    let directory_link = temp.path().join("directory-link");
    symlink(&directory, &directory_link).unwrap();
    assert_eq!(
        store
            .backup(directory_link.join("new.db"))
            .unwrap_err()
            .code,
        "invalid_path"
    );
    assert_eq!(
        store
            .backup(directory.join("../escaped.db"))
            .unwrap_err()
            .code,
        "invalid_path"
    );
    assert!(!temp.path().join("escaped.db").exists());
}
