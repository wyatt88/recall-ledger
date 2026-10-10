use recall_ledger::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::{Arc, Barrier};
use tempfile::TempDir;
use uuid::Uuid;

struct Fixture {
    _dir: TempDir,
    path: std::path::PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.sqlite");
        let store = Store::open(&path).unwrap();
        Self {
            _dir: dir,
            path,
            store,
        }
    }
    fn run(&mut self, method: &str, mut v: Value) -> Value {
        if ![
            "inspect",
            "maintenance_job",
            "maintenance_status",
            "operation",
            "recall",
        ]
        .contains(&method)
        {
            v["idempotency_key"] = json!(Uuid::new_v4().to_string());
        }
        self.store.execute("main", method, v).unwrap()
    }
    fn remember(&mut self, text: &str) -> String {
        self.run("remember", json!({"body":text}))["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn recipe(&mut self, inputs: &[&str]) -> Value {
        self.run(
            "summary_create",
            json!({"inputs":inputs,"processor":"extractive-v1","max_chars":1000}),
        )
    }
    fn claim(&mut self) -> Value {
        self.run("maintenance_claim", json!({"worker":"test-worker"}))
    }
    fn complete(&mut self, claim: &Value, text: &str) -> Value {
        self.run(
            "maintenance_complete",
            json!({"task_id":claim["task_id"],"lease_token":claim["lease_token"],"body":text}),
        )
    }
}

fn lease_args(claim: &Value) -> Value {
    json!({"task_id":claim["task_id"],"lease_token":claim["lease_token"]})
}

#[test]
fn queued_summary_survives_restart_and_publishes_stable_output() {
    let mut f = Fixture::new();
    let source = f.remember("Rust is the project language.");
    let recipe = f.recipe(&[&source]);
    assert_eq!(
        f.store
            .execute("main", "inspect", json!({"id":recipe["output_id"]}))
            .unwrap_err()
            .code,
        "not_found"
    );
    f.store = Store::open(&f.path).unwrap();
    let claim = f.claim();
    assert_eq!(claim["task_id"], recipe["task_id"]);
    assert!(!claim.to_string().contains("Rust is"));
    let job = f.run("maintenance_job", lease_args(&claim));
    assert_eq!(job["inputs"][0]["body"], "Rust is the project language.");
    let done = f.complete(&claim, "The project uses Rust.");
    assert_eq!(done["id"], recipe["output_id"]);
    assert_eq!(done["revision"], 1);
    assert_eq!(
        f.run("inspect", json!({"id":done["id"]}))["body"],
        "The project uses Rust."
    );
    assert_eq!(f.claim()["claimed"], false);
}

#[test]
fn first_generation_is_fenced_when_source_changes_before_publication() {
    let mut f = Fixture::new();
    let source = f.remember("Version one");
    let recipe = f.recipe(&[&source]);
    let old = f.claim();
    f.run(
        "revise",
        json!({"id":source,"expected_revision":1,"body":"Version two"}),
    );
    assert_eq!(
        f.store
            .execute("main", "maintenance_job", lease_args(&old))
            .unwrap_err()
            .code,
        "lease_conflict"
    );
    let stale = f.store.execute(
        "main",
        "maintenance_complete",
        json!({"task_id":old["task_id"],"lease_token":old["lease_token"],
            "body":"Stale summary","idempotency_key":"old-completion"}),
    );
    assert_eq!(stale.unwrap_err().code, "lease_conflict");
    let new = f.claim();
    assert_eq!(new["output_id"], recipe["output_id"]);
    assert_eq!(new["dependencies"][0]["revision"], 2);
    assert!(new["generation"].as_i64().unwrap() > old["generation"].as_i64().unwrap());
    f.complete(&new, "Current version two summary");
}

#[test]
fn source_revision_repairs_two_summary_levels_in_dependency_order() {
    let mut f = Fixture::new();
    let source = f.remember("Release date is May.");
    let first = f.recipe(&[&source]);
    let c1 = f.claim();
    f.complete(&c1, "Release May");
    let first_id = first["output_id"].as_str().unwrap();
    let second = f.recipe(&[first_id]);
    let c2 = f.claim();
    f.complete(&c2, "Schedule contains a May release");

    f.run(
        "revise",
        json!({"id":source,"expected_revision":1,"body":"Release date is June."}),
    );
    assert_eq!(
        f.store
            .execute("main", "inspect", json!({"id":second["output_id"]}))
            .unwrap_err()
            .code,
        "stale_dependency"
    );
    let repair1 = f.claim();
    assert_eq!(repair1["output_id"], first["output_id"]);
    assert_eq!(f.claim()["claimed"], false, "downstream waits for upstream");
    let out1 = f.complete(&repair1, "Release June");
    assert_eq!(out1["revision"], 2);
    let repair2 = f.claim();
    assert_eq!(repair2["output_id"], second["output_id"]);
    assert_eq!(repair2["dependencies"][0]["revision"], 2);
    let out2 = f.complete(&repair2, "Schedule contains a June release");
    assert_eq!(out2["revision"], 2);
    let read = f.run("inspect", json!({"id":second["output_id"]}));
    assert_eq!(read["dependencies"][0]["revision"], 2);
    assert!(!read["body"].as_str().unwrap().contains("May"));
}

#[test]
fn two_workers_cannot_claim_the_same_live_task() {
    let mut f = Fixture::new();
    let source = f.remember("A shared source");
    f.recipe(&[&source]);
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|i| {
            let path = f.path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                barrier.wait();
                store
                    .execute(
                        "main",
                        "maintenance_claim",
                        json!({"worker":format!("worker-{i}"),"idempotency_key":format!("claim-{i}")}),
                    )
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|v| v["claimed"] == true).count(), 1);
}

#[test]
fn expired_worker_lease_is_reclaimed_and_old_token_is_rejected() {
    let mut f = Fixture::new();
    let source = f.remember("Recoverable task");
    f.recipe(&[&source]);
    let old = f.claim();
    // Move only fixture lease time; avoid wall-clock sleeps and race-dependent tests.
    Connection::open(&f.path)
        .unwrap()
        .execute(
            "UPDATE maintenance_tasks SET lease_until=0 WHERE id=?1",
            [old["task_id"].as_str().unwrap()],
        )
        .unwrap();
    f.store = Store::open(&f.path).unwrap();
    let new = f.claim();
    assert_eq!(new["task_id"], old["task_id"]);
    assert_ne!(new["lease_token"], old["lease_token"]);
    assert_eq!(new["claim_count"], 2);
    assert_eq!(
        f.store
            .execute("main", "maintenance_job", lease_args(&old))
            .unwrap_err()
            .code,
        "lease_conflict"
    );
    f.complete(&new, "Recovered summary");
}

#[test]
fn retract_cancels_pending_and_running_recipes_without_receipt_body_leaks() {
    let mut f = Fixture::new();
    let sensitive = "private-fixture-value";
    let source = f.remember(sensitive);
    let recipe = f.recipe(&[&source]);
    let claim = f
        .store
        .execute(
            "main",
            "maintenance_claim",
            json!({"worker":"worker","idempotency_key":"recoverable-claim"}),
        )
        .unwrap();
    f.run("retract", json!({"id":source,"expected_revision":1}));
    assert_eq!(
        f.store
            .execute("main", "maintenance_job", lease_args(&claim))
            .unwrap_err()
            .code,
        "lease_conflict"
    );
    let recovered = f.run(
        "operation",
        json!({"method":"maintenance_claim","idempotency_key":"recoverable-claim"}),
    );
    assert!(!recovered.to_string().contains(sensitive));
    assert_eq!(f.claim()["claimed"], false);
    let status = f.run("maintenance_status", json!({}));
    assert_eq!(status["counts"]["cancelled"], 1);
    let rejected = f.store.execute(
        "main",
        "maintenance_retry",
        json!({"task_id":recipe["task_id"],"idempotency_key":"forbidden-retry"}),
    );
    assert_eq!(rejected.unwrap_err().code, "task_state_conflict");
}

#[test]
fn retract_published_output_is_terminal_and_old_complete_receipt_cannot_revive_it() {
    let mut f = Fixture::new();
    let source = f.remember("Original source");
    f.recipe(&[&source]);
    let claim = f.claim();
    let args = json!({"task_id":claim["task_id"],"lease_token":claim["lease_token"],
        "body":"summary-body-secret-fixture","idempotency_key":"successful-completion"});
    let done = f
        .store
        .execute("main", "maintenance_complete", args.clone())
        .unwrap();
    f.run("retract", json!({"id":done["id"],"expected_revision":1}));
    let receipt = f
        .store
        .execute("main", "maintenance_complete", args)
        .unwrap();
    assert_eq!(receipt, done);
    assert!(!receipt.to_string().contains("summary-body-secret-fixture"));
    assert_eq!(
        f.store
            .execute("main", "inspect", json!({"id":done["id"],"history":true}))
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(f.claim()["claimed"], false);
}

#[test]
fn retract_one_of_several_inputs_withdraws_both_published_and_pending_descendants() {
    let mut f = Fixture::new();
    let removed = f.remember("Withdrawn confidential input");
    let remaining = f.remember("Other input that remains available");
    let first = f.recipe(&[&removed, &remaining]);
    let claim = f.claim();
    f.complete(&claim, "A combined summary");
    let second = f.recipe(&[first["output_id"].as_str().unwrap()]);
    let late = f.claim();
    f.run("retract", json!({"id":removed,"expected_revision":1}));
    for output in [&first["output_id"], &second["output_id"]] {
        assert_eq!(
            f.store
                .execute("main", "inspect", json!({"id":output,"history":true}))
                .unwrap_err()
                .code,
            "not_found"
        );
    }
    assert_eq!(
        f.store
            .execute("main", "maintenance_job", lease_args(&late))
            .unwrap_err()
            .code,
        "lease_conflict"
    );
    assert_eq!(f.claim()["claimed"], false);
    assert_eq!(
        f.run("maintenance_status", json!({}))["counts"]["cancelled"],
        2
    );
    assert_eq!(
        f.run("inspect", json!({"id":remaining}))["body"],
        "Other input that remains available"
    );
}

#[test]
fn failures_back_off_stop_after_five_and_explicit_retry_is_durable() {
    let mut f = Fixture::new();
    let source = f.remember("Retry source");
    let recipe = f.recipe(&[&source]);
    for attempt in 1..=5 {
        let claim = f.claim();
        assert_eq!(claim["claimed"], true);
        let failed = f.run(
            "maintenance_fail",
            json!({"task_id":claim["task_id"],"lease_token":claim["lease_token"],
                "error_code":"provider_unavailable","retryable":true}),
        );
        assert_eq!(failed["attempts"], attempt);
        assert_eq!(
            failed["state"],
            if attempt < 5 { "pending" } else { "failed" }
        );
        assert_eq!(f.claim()["claimed"], false);
        Connection::open(&f.path)
            .unwrap()
            .execute(
                "UPDATE maintenance_tasks SET next_attempt_at=0 WHERE id=?1",
                [recipe["task_id"].as_str().unwrap()],
            )
            .unwrap();
    }
    let status = f.run("maintenance_status", json!({}));
    assert_eq!(status["counts"]["failed"], 1);
    f.run("maintenance_retry", json!({"task_id":recipe["task_id"]}));
    f.store = Store::open(&f.path).unwrap();
    let claim = f.claim();
    assert_eq!(claim["attempts"], 0);
    assert_eq!(claim["generation"], 2);
    f.complete(&claim, "Recovered after explicit retry");
}

#[test]
fn namespace_and_managed_output_edit_guards_are_enforced() {
    let mut f = Fixture::new();
    let source = f.remember("Scope source");
    f.recipe(&[&source]);
    let claim = f.claim();
    assert_eq!(
        f.store
            .execute("other", "maintenance_job", lease_args(&claim))
            .unwrap_err()
            .code,
        "not_found"
    );
    let done = f.complete(&claim, "Managed summary");
    assert_eq!(
        f.store
            .execute(
                "main",
                "revise",
                json!({
                    "id":done["id"],"expected_revision":1,"body":"Unauthorized managed edit",
                    "idempotency_key":"managed-edit"
                })
            )
            .unwrap_err()
            .code,
        "managed_record"
    );
}

#[test]
fn manual_artifact_in_the_middle_is_never_silently_recomputed() {
    let mut f = Fixture::new();
    let source = f.remember("Source old");
    let a = f.recipe(&[&source]);
    let claim_a = f.claim();
    f.complete(&claim_a, "A old");
    let prep = f.run(
        "prepare",
        json!({"inputs":[a["output_id"]],"processor":"manual-only"}),
    );
    let b = f.run(
        "publish",
        json!({"job_id":prep["job_id"],"body":"B manually checked"}),
    );
    f.recipe(&[b["id"].as_str().unwrap()]);
    let claim_c = f.claim();
    f.complete(&claim_c, "C old");
    f.run(
        "revise",
        json!({"id":source,"expected_revision":1,"body":"Source new"}),
    );
    let repair_a = f.claim();
    assert_eq!(repair_a["output_id"], a["output_id"]);
    f.complete(&repair_a, "A new");
    assert_eq!(
        f.claim()["claimed"],
        false,
        "manual B remains stale so C must wait"
    );
    assert_eq!(
        f.store
            .execute("main", "inspect", json!({"id":b["id"]}))
            .unwrap_err()
            .code,
        "stale_dependency"
    );
}

#[test]
fn managed_output_compare_and_swap_rejects_unexpected_revision_change() {
    let mut f = Fixture::new();
    let source = f.remember("Source v1");
    let recipe = f.recipe(&[&source]);
    let initial = f.claim();
    f.complete(&initial, "First summary");
    f.run(
        "revise",
        json!({"id":source,"expected_revision":1,"body":"Source v2"}),
    );
    let repair = f.claim();
    // Simulate an out-of-band database writer; public revise is blocked for managed outputs.
    Connection::open(&f.path)
        .unwrap()
        .execute(
            "UPDATE records SET revision=revision+1 WHERE id=?1",
            [recipe["output_id"].as_str().unwrap()],
        )
        .unwrap();
    assert_eq!(
        f.store
            .execute(
                "main",
                "maintenance_complete",
                json!({
                    "task_id":repair["task_id"],"lease_token":repair["lease_token"],
                    "body":"Must not overwrite","idempotency_key":"late-cas"
                })
            )
            .unwrap_err()
            .code,
        "revision_conflict"
    );
}

#[test]
fn recipe_and_summary_body_budgets_are_validated() {
    let mut f = Fixture::new();
    let a = f.remember(&"a".repeat(40_000));
    let b = f.remember(&"b".repeat(40_000));
    assert_eq!(
        f.store
            .execute(
                "main",
                "summary_create",
                json!({
                    "inputs":[a,b],"processor":"extractive-v1","idempotency_key":"too-large"
                })
            )
            .unwrap_err()
            .code,
        "resource_exhausted"
    );
    f.run(
        "summary_create",
        json!({"inputs":[a],"processor":"extractive-v1","max_chars":5}),
    );
    let claim = f.claim();
    assert_eq!(
        f.store
            .execute(
                "main",
                "maintenance_complete",
                json!({
                    "task_id":claim["task_id"],"lease_token":claim["lease_token"],
                    "body":"Six or more chars","idempotency_key":"over-summary-budget"
                })
            )
            .unwrap_err()
            .code,
        "invalid_request"
    );
}

#[test]
fn current_input_temporal_validity_is_checked_before_body_access_and_completion() {
    let mut f = Fixture::new();
    let source = f.remember("Temporary source");
    f.recipe(&[&source]);
    let claim = f.claim();
    // The production guard uses wall time. Move fixture expiry into the past
    // without revising the source to exercise that guard independently of callbacks.
    Connection::open(&f.path)
        .unwrap()
        .execute("UPDATE records SET valid_until=1 WHERE id=?1", [&source])
        .unwrap();
    assert_eq!(
        f.store
            .execute("main", "maintenance_job", lease_args(&claim))
            .unwrap_err()
            .code,
        "stale_dependency"
    );
    assert_eq!(
        f.store
            .execute(
                "main",
                "maintenance_complete",
                json!({
                    "task_id":claim["task_id"],"lease_token":claim["lease_token"],
                    "body":"Expired source summary","idempotency_key":"expired-completion"
                })
            )
            .unwrap_err()
            .code,
        "stale_dependency"
    );
}
