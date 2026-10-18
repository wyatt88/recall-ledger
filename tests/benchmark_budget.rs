use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONFIG: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn command(path: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_recall-bench-budget"));
    command.arg("--db").arg(path).args(args);
    command
}

fn cli(path: &Path, args: &[&str]) -> Output {
    command(path, args).output().unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn failure(output: Output) -> String {
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let response: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(response["status"], "error");
    response["error"]["code"].as_str().unwrap().to_owned()
}

fn reserve_args<'a>(id: &'a str, amount: &'a str) -> Vec<&'a str> {
    vec![
        "reserve",
        "--request-id",
        id,
        "--request-sha256",
        HASH,
        "--run-id",
        "run-1",
        "--configuration-id",
        CONFIG,
        "--provider-scope",
        "test-provider/account-1/project-1",
        "--operation",
        "generation",
        "--price-snapshot-id",
        "prices-locked-1",
        "--stage",
        "P2",
        "--ledger",
        "system_runtime",
        "--bound-usd",
        amount,
    ]
}

struct Fixture {
    _dir: TempDir,
    db: PathBuf,
}

impl Fixture {
    fn new(limit: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let db = dir.path().join("budget.sqlite");
        success(cli(
            &db,
            &["init", "--budget-id", "budget-1", "--limit-usd", limit],
        ));
        Self { _dir: dir, db }
    }

    fn reserve(&self, id: &str, amount: &str) -> Value {
        success(cli(&self.db, &reserve_args(id, amount)))
    }

    fn dispatch(&self, id: &str) -> Value {
        success(cli(&self.db, &["mark-dispatched", "--request-id", id]))
    }

    fn settle(&self, id: &str, amount: &str, event: &str, provider: &str) -> Output {
        cli(
            &self.db,
            &[
                "settle",
                "--request-id",
                id,
                "--actual-usd",
                amount,
                "--cost-event-id",
                event,
                "--provider-request-id",
                provider,
            ],
        )
    }

    fn snapshot(&self) -> Value {
        success(cli(&self.db, &["snapshot"]))
    }
}

#[test]
fn concurrent_processes_cannot_over_reserve() {
    let f = Fixture::new("10");
    let children: Vec<_> = (0..16)
        .map(|i| {
            command(&f.db, &reserve_args(&format!("request-{i}"), "3"))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut allowed = 0;
    let mut denied = 0;
    for child in children {
        let output = child.wait_with_output().unwrap();
        if output.status.success() {
            assert_eq!(success(output)["idempotent"], false);
            allowed += 1;
        } else {
            assert_eq!(failure(output), "budget_exhausted");
            denied += 1;
        }
    }
    assert_eq!((allowed, denied), (3, 13));
    let result = f.snapshot();
    assert_eq!(result["budget"]["committed_micro_usd"], "9000000");
    assert_eq!(result["budget"]["available_micro_usd"], "1000000");
}

#[test]
fn concurrent_same_request_allocates_and_grants_dispatch_only_once() {
    let f = Fixture::new("1");
    let children: Vec<_> = (0..8)
        .map(|_| {
            command(&f.db, &reserve_args("same-request", "1"))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let reservations = children
        .into_iter()
        .map(|child| success(child.wait_with_output().unwrap()))
        .filter(|value| value["idempotent"] == false)
        .count();
    assert_eq!(reservations, 1);
    let children: Vec<_> = (0..8)
        .map(|_| {
            command(&f.db, &["mark-dispatched", "--request-id", "same-request"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let permits = children
        .into_iter()
        .map(|child| success(child.wait_with_output().unwrap()))
        .filter(|value| value["dispatch_permit_granted"] == true)
        .count();
    assert_eq!(permits, 1);
    assert_eq!(f.snapshot()["budget"]["dispatched_unsettled_requests"], 1);
}

#[test]
fn lost_dispatch_ack_survives_reopen_without_freeing_unknown_charges() {
    let f = Fixture::new("5");
    f.reserve("lost-ack", "4");
    // Discard the successful caller's response as if it died before persisting
    // the acknowledgement. The next CLI process sees the durable dispatch.
    let status = command(&f.db, &["mark-dispatched", "--request-id", "lost-ack"])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(f.dispatch("lost-ack")["dispatch_permit_granted"], false);
    assert_eq!(
        failure(cli(&f.db, &["cancel", "--request-id", "lost-ack"])),
        "invalid_transition"
    );
    success(cli(&f.db, &["close"]));
    let reopened = success(cli(&f.db, &["reopen"]));
    assert_eq!(reopened["budget"]["available_micro_usd"], "1000000");
    assert_eq!(reopened["budget"]["accounting_status"], "unsettled");
    assert_eq!(f.reserve("lost-ack", "4")["idempotent"], true);
    assert_eq!(
        failure(cli(&f.db, &reserve_args("new-attempt", "2"))),
        "budget_exhausted"
    );
    success(f.settle("lost-ack", "3", "event-1", "provider-1"));
    let replay = success(f.settle("lost-ack", "3.000000", "event-1", "provider-1"));
    assert_eq!(replay["idempotent"], true);
    assert_eq!(replay["budget"]["settled_micro_usd"], "3000000");
    assert_eq!(replay["budget"]["available_micro_usd"], "2000000");
    assert_eq!(f.dispatch("lost-ack")["dispatch_permit_granted"], false);
}

#[test]
fn terminated_waiting_process_leaves_no_partial_reservation() {
    let f = Fixture::new("2");
    f.reserve("committed", "1");
    let connection = rusqlite::Connection::open(&f.db).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut waiting = command(&f.db, &reserve_args("terminated", "1"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(waiting.try_wait().unwrap().is_none());
    waiting.kill().unwrap();
    waiting.wait().unwrap();
    connection.execute_batch("ROLLBACK").unwrap();
    let result = f.snapshot();
    assert_eq!(result["budget"]["reserved_requests"], 1);
    assert_eq!(result["budget"]["committed_micro_usd"], "1000000");
    assert_eq!(f.reserve("committed", "1")["idempotent"], true);
    assert_eq!(f.reserve("terminated", "1")["idempotent"], false);
}

#[test]
fn abrupt_exit_with_uncommitted_sqlite_changes_preserves_committed_reservations() {
    const CHILD_DB: &str = "RECALL_BENCH_BUDGET_CRASH_TEST_DB";
    if let Some(path) = std::env::var_os(CHILD_DB) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch(
                "PRAGMA synchronous=FULL;
                 BEGIN IMMEDIATE;
                 UPDATE requests SET state='cancelled' WHERE request_id='committed';
                 UPDATE budget SET limit_micro_usd=9000000;",
            )
            .unwrap();
        // Exit without running Connection/Transaction destructors, as with a
        // abruptly terminated controller. This is not a machine-power-loss test.
        std::process::exit(73);
    }
    let f = Fixture::new("2");
    f.reserve("committed", "1");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "abrupt_exit_with_uncommitted_sqlite_changes_preserves_committed_reservations",
            "--nocapture",
        ])
        .env(CHILD_DB, &f.db)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(73));
    let snapshot = f.snapshot();
    assert_eq!(snapshot["budget"]["limit_micro_usd"], "2000000");
    assert_eq!(snapshot["budget"]["committed_micro_usd"], "1000000");
    assert_eq!(f.reserve("committed", "1")["request"]["state"], "reserved");
}

#[test]
fn concurrent_first_initializations_cannot_replace_the_winning_limit() {
    let directory = tempfile::tempdir().unwrap();
    // Let init create and protect a previously absent dedicated directory.
    let path = directory.path().join("private").join("budget.sqlite");
    let children: Vec<_> = (1..=8)
        .map(|amount| {
            command(
                &path,
                &[
                    "init",
                    "--budget-id",
                    "budget-1",
                    "--limit-usd",
                    &amount.to_string(),
                ],
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
        })
        .collect();
    let mut winning_limit = None;
    for child in children {
        let output = child.wait_with_output().unwrap();
        if output.status.success() {
            assert!(winning_limit.is_none());
            winning_limit = Some(success(output)["budget"]["limit_micro_usd"].clone());
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            assert_eq!(failure(output), "initialization_conflict", "{stderr}");
        }
    }
    assert_eq!(
        success(cli(&path, &["snapshot"]))["budget"]["limit_micro_usd"],
        winning_limit.unwrap()
    );
}

#[test]
fn duplicate_request_rejects_every_changed_billing_or_attribution_field() {
    let f = Fixture::new("20");
    f.reserve("one", "2");
    for (key, changed) in [
        ("--request-sha256", CONFIG),
        ("--run-id", "another-run"),
        ("--configuration-id", HASH),
        ("--provider-scope", "another-account"),
        ("--operation", "embedding"),
        ("--price-snapshot-id", "another-price"),
        ("--stage", "P3"),
        ("--ledger", "evaluation"),
        ("--bound-usd", "3"),
    ] {
        let mut args = reserve_args("one", "2");
        let position = args.iter().position(|field| *field == key).unwrap();
        args[position + 1] = changed;
        assert_eq!(failure(cli(&f.db, &args)), "idempotency_conflict", "{key}");
    }
    assert_eq!(f.snapshot()["budget"]["committed_micro_usd"], "2000000");
}

#[test]
fn overrun_is_recorded_and_permanently_halts_new_reservations_and_dispatch() {
    let f = Fixture::new("5");
    f.reserve("overrun", "2");
    f.reserve("already-dispatched", "1");
    f.reserve("not-dispatched", "1");
    f.dispatch("overrun");
    f.dispatch("already-dispatched");
    let recorded = success(f.settle("overrun", "6", "event-1", "provider-1"));
    assert_eq!(recorded["budget"]["status"], "halted");
    assert_eq!(recorded["budget"]["settled_micro_usd"], "6000000");
    assert_eq!(recorded["budget"]["committed_micro_usd"], "8000000");
    assert_eq!(recorded["budget"]["available_micro_usd"], "0");
    assert_eq!(
        failure(cli(&f.db, &reserve_args("after-overrun", "1"))),
        "budget_halted"
    );
    assert_eq!(
        failure(cli(
            &f.db,
            &["mark-dispatched", "--request-id", "not-dispatched"]
        )),
        "budget_halted"
    );
    success(cli(&f.db, &["cancel", "--request-id", "not-dispatched"]));
    success(f.settle("already-dispatched", "0.5", "event-2", "provider-2"));
    success(cli(&f.db, &["close"]));
    assert_eq!(
        success(cli(&f.db, &["reopen"]))["budget"]["status"],
        "halted"
    );
    let snapshot = f.snapshot();
    assert_eq!(snapshot["budget"]["settled_micro_usd"], "6500000");
    assert_eq!(snapshot["budget"]["provider_invoice_cap_enforced"], false);
}

#[test]
fn final_cost_events_and_provider_requests_are_deduplicated() {
    let f = Fixture::new("10");
    for id in ["first", "second", "third"] {
        f.reserve(id, "2");
        f.dispatch(id);
    }
    success(f.settle("first", "1", "event-1", "provider-1"));
    assert_eq!(
        failure(f.settle("second", "1", "event-1", "provider-2")),
        "duplicate_cost_event"
    );
    assert_eq!(
        failure(f.settle("second", "1", "event-2", "provider-1")),
        "duplicate_provider_request"
    );
    assert_eq!(
        failure(f.settle("first", "0.5", "event-1", "provider-1")),
        "idempotency_conflict"
    );
    assert_eq!(
        failure(f.settle("third", "1", "event-3", "provider-1")),
        "duplicate_provider_request"
    );
    let result = f.snapshot();
    assert_eq!(result["budget"]["settled_requests"], 1);
    assert_eq!(result["budget"]["dispatched_unsettled_requests"], 2);
    assert_eq!(result["budget"]["committed_micro_usd"], "5000000");
}

#[test]
fn initialization_and_close_never_reset_spent_money_or_existing_request_ids() {
    let f = Fixture::new("3");
    f.reserve("spent", "2");
    f.dispatch("spent");
    success(f.settle("spent", "2", "event", "provider"));
    let repeat = success(cli(
        &f.db,
        &["init", "--budget-id", "budget-1", "--limit-usd", "3.000000"],
    ));
    assert_eq!(repeat["idempotent"], true);
    assert_eq!(repeat["budget"]["settled_micro_usd"], "2000000");
    for args in [
        ["init", "--budget-id", "budget-1", "--limit-usd", "4"],
        ["init", "--budget-id", "new-budget", "--limit-usd", "3"],
        ["init", "--budget-id", "budget-1", "--limit-usd", "1"],
    ] {
        assert_eq!(failure(cli(&f.db, &args)), "initialization_conflict");
    }
    f.reserve("cancel-me", "1");
    let closed = success(cli(&f.db, &["close"]));
    assert_eq!(closed["budget"]["status"], "closed");
    assert_eq!(
        failure(cli(&f.db, &reserve_args("closed-new", "1"))),
        "budget_closed"
    );
    assert_eq!(
        failure(cli(
            &f.db,
            &["mark-dispatched", "--request-id", "cancel-me"]
        )),
        "budget_closed"
    );
    let cancel = success(cli(&f.db, &["cancel", "--request-id", "cancel-me"]));
    assert_eq!(cancel["budget"]["available_micro_usd"], "1000000");
    assert_eq!(
        success(cli(&f.db, &["cancel", "--request-id", "cancel-me"]))["idempotent"],
        true
    );
    success(cli(&f.db, &["reopen"]));
    assert_eq!(f.reserve("cancel-me", "1")["request"]["state"], "cancelled");
    assert_eq!(
        failure(cli(
            &f.db,
            &["mark-dispatched", "--request-id", "cancel-me"]
        )),
        "invalid_transition"
    );
    assert_eq!(f.snapshot()["budget"]["settled_micro_usd"], "2000000");
}

#[test]
fn money_parsing_rejects_rounding_signs_exponents_and_overflow() {
    let f = Fixture::new("1");
    for (i, amount) in [
        "-1",
        "+1",
        "NaN",
        "inf",
        "1e0",
        ".5",
        "1.",
        "1.0000001",
        "1,00",
        " 1",
        "1 ",
        "9223372036854.775808",
        "999999999999999999999999",
        "0",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            failure(cli(&f.db, &reserve_args(&format!("bad-{i}"), amount))),
            "invalid_amount",
            "{amount}"
        );
    }
    let exact = f.reserve("micro", "0.000001");
    assert_eq!(exact["request"]["bound_micro_usd"], "1");
    f.dispatch("micro");
    let zero = success(f.settle("micro", "0", "zero-charge", "zero-provider"));
    assert_eq!(zero["budget"]["settled_micro_usd"], "0");
    assert_eq!(zero["budget"]["available_micro_usd"], "1000000");
}

#[test]
fn accumulated_real_overruns_cannot_overflow_sqlite_integer_totals() {
    let f = Fixture::new("1");
    for id in ["one", "two", "three"] {
        f.reserve(id, "0.1");
        f.dispatch(id);
    }
    for (i, id) in ["one", "two", "three"].iter().enumerate() {
        success(f.settle(
            id,
            "9223372036854.775807",
            &format!("event-{i}"),
            &format!("provider-{i}"),
        ));
    }
    let snapshot = f.snapshot();
    assert_eq!(
        snapshot["budget"]["settled_micro_usd"],
        "27670116110564327421"
    );
    assert_eq!(snapshot["budget"]["settled_requests"], 3);
    assert_eq!(snapshot["budget"]["status"], "halted");
}

#[test]
fn rejects_other_database_and_uses_wal_full_durability() {
    let f = Fixture::new("1");
    let connection = rusqlite::Connection::open(&f.db).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    // FULL is connection-local; the CLI sets it on every open. The source
    // persists the application identity to prevent opening a RecallLedger DB.
    let other = f._dir.path().join("other.sqlite");
    let other_connection = rusqlite::Connection::open(&other).unwrap();
    other_connection
        .execute_batch("CREATE TABLE unrelated(value TEXT)")
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(
        failure(cli(
            &other,
            &["init", "--budget-id", "budget-1", "--limit-usd", "1"]
        )),
        "schema_mismatch"
    );
    assert_eq!(
        other_connection
            .query_row("SELECT count(*) FROM unrelated", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[cfg(unix)]
#[test]
fn private_paths_reject_symlinks_hardlinks_and_permissive_modes() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = Fixture::new("1");
    assert_eq!(f.db.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    let linked = f._dir.path().join("linked.sqlite");
    symlink(&f.db, &linked).unwrap();
    assert_eq!(failure(cli(&linked, &["snapshot"])), "invalid_path");
    std::fs::remove_file(&linked).unwrap();
    std::fs::hard_link(&f.db, &linked).unwrap();
    assert_eq!(failure(cli(&f.db, &["snapshot"])), "invalid_path");
    std::fs::remove_file(&linked).unwrap();
    let sidecar = f._dir.path().join("budget.sqlite-wal");
    symlink(&f.db, &sidecar).unwrap();
    assert_eq!(failure(cli(&f.db, &["snapshot"])), "invalid_path");
    std::fs::remove_file(&sidecar).unwrap();
    std::fs::set_permissions(&f.db, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(failure(cli(&f.db, &["snapshot"])), "invalid_path");
    std::fs::set_permissions(&f.db, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(f._dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(failure(cli(&f.db, &["snapshot"])), "invalid_path");
}

#[cfg(unix)]
#[test]
fn operation_lock_precedes_sidecar_validation_and_is_released_after_errors() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new("1");
    let lock_path = f._dir.path().join("budget.sqlite.operation.lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    // While a cooperating connection owns the lock, its sidecars can be in
    // transition. A waiting CLI must not inspect them before obtaining the lock.
    let sidecar = f._dir.path().join("budget.sqlite-wal");
    symlink(&f.db, &sidecar).unwrap();
    let mut waiting = command(&f.db, &["snapshot"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(waiting.try_wait().unwrap().is_none());
    std::fs::remove_file(&sidecar).unwrap();
    fs2::FileExt::unlock(&lock).unwrap();
    success(waiting.wait_with_output().unwrap());
    // A failed transaction must release the lock too; it must never reset the
    // original limit or remove/replace the shared lock inode.
    assert_eq!(
        failure(cli(
            &f.db,
            &["init", "--budget-id", "budget-1", "--limit-usd", "2"]
        )),
        "initialization_conflict"
    );
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    fs2::FileExt::unlock(&lock).unwrap();
    assert_eq!(f.snapshot()["budget"]["limit_micro_usd"], "1000000");
}

#[cfg(unix)]
#[test]
fn operation_lock_rejects_symlinks_hardlinks_and_permissive_modes_without_repair() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let f = Fixture::new("1");
    let lock_path = f._dir.path().join("budget.sqlite.operation.lock");
    let target = f._dir.path().join("lock-target");
    std::fs::rename(&lock_path, &target).unwrap();
    symlink(&target, &lock_path).unwrap();
    assert_eq!(failure(cli(&f.db, &["snapshot"])), "invalid_path");
    assert!(lock_path
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());
    std::fs::remove_file(&lock_path).unwrap();
    std::fs::hard_link(&target, &lock_path).unwrap();
    assert_eq!(failure(cli(&f.db, &["snapshot"])), "invalid_path");
    std::fs::remove_file(&lock_path).unwrap();
    std::fs::rename(&target, &lock_path).unwrap();
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(failure(cli(&f.db, &["snapshot"])), "invalid_path");
    assert_eq!(
        lock_path.metadata().unwrap().permissions().mode() & 0o777,
        0o644
    );
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    success(cli(&f.db, &["snapshot"]));
}
