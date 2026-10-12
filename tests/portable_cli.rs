use recall_ledger::Store;
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn cli(db: &Path, namespace: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
        .arg("--db")
        .arg(db)
        .args(["--namespace", namespace])
        .args(args)
        .output()
        .unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn error(output: Output) -> String {
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    error["error"]["code"].as_str().unwrap().to_owned()
}

struct Fixture {
    dir: TempDir,
    source: std::path::PathBuf,
    target: std::path::PathBuf,
    bundle: std::path::PathBuf,
    source_binding: std::path::PathBuf,
    plan: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.db");
        let target = dir.path().join("target.db");
        let bundle = dir.path().join("bundle.json");
        let source_binding = dir.path().join("source-binding.json");
        let plan = dir.path().join("plan.json");
        let mut store = Store::open(&source).unwrap();
        store
            .execute(
                "alice",
                "remember",
                json!({"body":"部署窗口是星期四。","idempotency_key":"source-1"}),
            )
            .unwrap();
        let binding = store.execute("alice", "binding", json!({})).unwrap();
        std::fs::write(&source_binding, serde_json::to_vec(&binding).unwrap()).unwrap();
        Self {
            dir,
            source,
            target,
            bundle,
            source_binding,
            plan,
        }
    }

    fn export(&self) -> Value {
        success(cli(
            &self.source,
            "alice",
            &[
                "bundle",
                "export",
                "--output",
                self.bundle.to_str().unwrap(),
            ],
        ))
    }

    fn plan(&self) -> Value {
        success(cli(
            &self.target,
            "bob",
            &[
                "bundle",
                "plan",
                "--bundle",
                self.bundle.to_str().unwrap(),
                "--source-binding",
                self.source_binding.to_str().unwrap(),
                "--output",
                self.plan.to_str().unwrap(),
            ],
        ))
    }

    fn apply(&self, namespace: &str) -> Output {
        cli(
            &self.target,
            namespace,
            &[
                "bundle",
                "apply",
                "--bundle",
                self.bundle.to_str().unwrap(),
                "--plan",
                self.plan.to_str().unwrap(),
                "--idempotency-key",
                "reviewed-import-1",
            ],
        )
    }
}

#[test]
fn reviewed_cli_transfer_is_private_read_only_before_apply_and_replayable() {
    let fixture = Fixture::new();
    fixture.export();
    let mut target = Store::open(&fixture.target).unwrap();
    let before = target.execute("bob", "status", json!({})).unwrap();
    let preview = fixture.plan();
    assert_eq!(preview["review"]["preview"]["can_apply"], true);
    let after = target.execute("bob", "status", json!({})).unwrap();
    assert_eq!(before["commit_token"], after["commit_token"]);
    assert_eq!(after["records"], 0);
    let receipt = success(fixture.apply("bob"));
    assert_eq!(receipt["applied"], true);
    assert_eq!(success(fixture.apply("bob")), receipt);
    assert!(!receipt.to_string().contains("星期四"));
    let recalled = target.execute("bob", "recall", json!({})).unwrap();
    assert!(recalled.to_string().contains("星期四"));
    assert_eq!(
        target.execute("other", "status", json!({})).unwrap()["records"],
        0
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in [&fixture.bundle, &fixture.plan] {
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}

#[test]
fn reviewed_cli_transfer_rejects_target_drift_and_wrong_namespace() {
    let fixture = Fixture::new();
    fixture.export();
    fixture.plan();
    assert_eq!(error(fixture.apply("wrong")), "binding_mismatch");
    let mut target = Store::open(&fixture.target).unwrap();
    target
        .execute(
            "bob",
            "remember",
            json!({"body":"a concurrent target write","idempotency_key":"local-1"}),
        )
        .unwrap();
    assert_eq!(error(fixture.apply("bob")), "target_changed");
    assert_eq!(
        target.execute("bob", "status", json!({})).unwrap()["records"],
        1
    );
}

#[test]
fn cli_rejects_changed_bundle_and_never_overwrites_output() {
    let fixture = Fixture::new();
    fixture.export();
    fixture.plan();
    let bytes = std::fs::read(&fixture.bundle).unwrap();
    assert_eq!(
        error(cli(
            &fixture.source,
            "alice",
            &[
                "bundle",
                "export",
                "--output",
                fixture.bundle.to_str().unwrap()
            ],
        )),
        "invalid_path"
    );
    assert_eq!(std::fs::read(&fixture.bundle).unwrap(), bytes);
    let mut bundle: Value = serde_json::from_slice(&bytes).unwrap();
    bundle["payload"]["records"][0]["body"] = json!("tampered text");
    std::fs::write(&fixture.bundle, serde_json::to_vec(&bundle).unwrap()).unwrap();
    assert_eq!(error(fixture.apply("bob")), "digest_mismatch");
    assert_eq!(
        Store::open(&fixture.target)
            .unwrap()
            .execute("bob", "status", json!({}))
            .unwrap()["records"],
        0
    );
}

#[cfg(unix)]
#[test]
fn cli_rejects_symlink_inputs() {
    let fixture = Fixture::new();
    fixture.export();
    fixture.plan();
    let actual = fixture.dir.path().join("actual-bundle.json");
    std::fs::rename(&fixture.bundle, &actual).unwrap();
    std::os::unix::fs::symlink(&actual, &fixture.bundle).unwrap();
    assert_eq!(error(fixture.apply("bob")), "invalid_file");
}

#[test]
fn schema_three_upgrade_preserves_identity_and_data() {
    let fixture = Fixture::new();
    let before = Store::open(&fixture.source)
        .unwrap()
        .execute("alice", "binding", json!({}))
        .unwrap();
    let connection = rusqlite::Connection::open(&fixture.source).unwrap();
    connection
        .execute_batch(
            "DROP TABLE portable_origins;
             DROP TABLE portable_watermarks;
             PRAGMA user_version=3;",
        )
        .unwrap();
    drop(connection);
    let mut upgraded = Store::open(&fixture.source).unwrap();
    assert_eq!(
        upgraded.execute("alice", "binding", json!({})).unwrap(),
        before
    );
    assert_eq!(
        upgraded.execute("alice", "status", json!({})).unwrap()["schema_version"],
        4
    );
    assert!(upgraded
        .execute("alice", "recall", json!({}))
        .unwrap()
        .to_string()
        .contains("星期四"));
    let bundle = upgraded
        .execute("alice", "bundle_export", json!({}))
        .unwrap();
    assert_eq!(bundle["payload"]["source_binding"], before);
}
