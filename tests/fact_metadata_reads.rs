//! Public-intake regressions for delivery of structured scope and conditions.
use recall_ledger::{projection::read_projection, Store};
use serde_json::{json, Value};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const NS: &str = "qualified-memory";

struct Fixture {
    directory: TempDir,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("memory.sqlite3")).unwrap();
        Self { directory, store }
    }

    fn read(&mut self, method: &str, args: Value) -> Value {
        self.store
            .execute(NS, method, args)
            .unwrap_or_else(|error| panic!("{method} failed: {error}"))
    }

    fn accept(&mut self, event: &str, body: &str, key: &Value, revision: Option<i64>) -> Value {
        let pending = self.read(
            "candidate_submit",
            json!({
                "event_id":event,"body":body,"origin":"user_explicit",
                "source":"synthetic-public-intake","fact_key":key,
                "idempotency_key":format!("submit-{event}")
            }),
        );
        assert_eq!(pending["state"], "pending");
        let mut args = json!({
            "candidate_id":pending["candidate_id"],"decision":"accept",
            "idempotency_key":format!("accept-{event}")
        });
        if let Some(revision) = revision {
            args["expected_revision"] = json!(revision);
        }
        self.read("candidate_resolve", args)
    }
}

fn key() -> Value {
    json!({
        "entity":"项目甲","attribute":"deployment_permission","scope":"仅测试环境",
        "conditions":"只有用户复核后；金额 ≤ ¥35；生产环境禁止"
    })
}

fn metadata_from_markdown(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("fact_key").is_some())
        .collect()
}

#[test]
fn accepted_scope_and_conditions_survive_inspect_recall_job_and_export() {
    let mut f = Fixture::new();
    let fact_key = key();
    let accepted = f.accept("deployment", "允许部署", &fact_key, None);
    let id = &accepted["record_id"];
    let inspected = f.read("inspect", json!({"id":id}));
    assert_eq!(inspected["body"], "允许部署");
    assert_eq!(inspected["fact_key"], fact_key);
    assert_eq!(inspected["applicable_now"], true);
    assert_eq!(inspected["trust_boundary"], "retrieved_data");

    let recalled = f.read("recall", json!({"query":"部署","max_chars":4000}));
    assert_eq!(recalled["items"].as_array().unwrap().len(), 1);
    assert_eq!(recalled["items"][0]["fact_key"], fact_key);
    assert_eq!(recalled["manifest"]["records"][0]["id"], *id);
    assert_eq!(recalled["manifest"]["records"][0]["revision"], 1);
    assert_eq!(
        recalled["manifest"]["commit_token"],
        inspected["commit_token"]
    );
    assert_eq!(
        f.read("export", json!({}))["records"][0]["fact_key"],
        fact_key
    );

    let prepared = f.read(
        "prepare",
        json!({"inputs":[id],"processor":"manual-fixture","idempotency_key":"prepare"}),
    );
    let job = f.read("job", json!({"job_id":prepared["job_id"]}));
    assert_eq!(job["inputs"][0]["fact_key"], fact_key);
    assert_eq!(job["inputs"][0]["body"], "允许部署");
}

#[test]
fn different_conditions_remain_distinct_even_when_bodies_match() {
    let mut f = Fixture::new();
    let first_key = key();
    let first = f.accept("first", "允许部署", &first_key, None);
    let mut second_key = first_key.clone();
    second_key["conditions"] = json!("仅审批单 42 有效；审批单 43 无效");
    let second = f.accept("second", "允许部署", &second_key, None);
    assert_ne!(first["record_id"], second["record_id"]);
    let recalled = f.read("recall", json!({"query":"部署","max_chars":4000}));
    let items = recalled["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .any(|item| item["id"] == first["record_id"] && item["fact_key"] == first_key));
    assert!(items
        .iter()
        .any(|item| item["id"] == second["record_id"] && item["fact_key"] == second_key));
}

#[test]
fn qualifier_budget_defers_whole_items_and_counts_unicode_json_without_truncation() {
    let mut f = Fixture::new();
    let fact_key = json!({
        "entity":"甲","attribute":"动作","scope":"仅沙箱",
        "conditions":format!("需先复核 \"{}\"；绝不自动执行", "条款".repeat(20))
    });
    let body = "允许";
    f.accept("bounded", body, &fact_key, None);
    let body_chars = body.chars().count();
    let key_chars = fact_key.to_string().chars().count();
    let total = body_chars + key_chars;
    let withheld = f.read("recall", json!({"max_chars":total-1}));
    assert_eq!(withheld["items"], json!([]));
    assert_eq!(withheld["manifest"]["records"], json!([]));
    assert_eq!(withheld["coverage"]["deferred"], 1);
    assert_eq!(withheld["coverage"]["status"], "budget_limited");
    assert_eq!(withheld["budget"]["used_chars"], 0);

    let exact = f.read("recall", json!({"max_chars":total}));
    assert_eq!(exact["items"][0]["body"], body);
    assert_eq!(exact["items"][0]["fact_key"], fact_key);
    assert_eq!(exact["budget"]["used_chars"], total);
    assert_eq!(exact["budget"]["body_chars"], body_chars);
    assert_eq!(exact["budget"]["fact_key_chars"], key_chars);
    assert_eq!(exact["budget"]["bodies_only"], false);
    assert_eq!(exact["budget"]["scope"], "body_and_fact_key");
    assert_eq!(exact["coverage"]["deferred"], 0);
}

#[test]
fn unkeyed_records_explicitly_report_null_without_inventing_conditions() {
    let mut f = Fixture::new();
    let remembered = f.read(
        "remember",
        json!({"body":"原文","idempotency_key":"unkeyed"}),
    );
    let inspected = f.read("inspect", json!({"id":remembered["id"]}));
    assert_eq!(inspected.get("fact_key"), Some(&Value::Null));
    let recalled = f.read("recall", json!({"max_chars":2}));
    assert_eq!(recalled["items"][0].get("fact_key"), Some(&Value::Null));
    assert_eq!(recalled["budget"]["used_chars"], 2);
    assert_eq!(recalled["budget"]["fact_key_chars"], 0);
}

#[test]
fn projection_binds_complete_metadata_and_refreshes_after_revision_and_withdrawal() {
    let mut f = Fixture::new();
    let fact_key = key();
    let accepted = f.accept("first", "允许部署", &fact_key, None);
    let directory = f.directory.path().join("projection");
    f.store.project(NS, &directory).unwrap();
    let first = f.store.read_current_projection(NS, &directory).unwrap();
    let metadata = metadata_from_markdown(first["body"].as_str().unwrap());
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0]["fact_key"], fact_key);
    assert_eq!(metadata[0]["valid_from"], Value::Null);
    assert_eq!(metadata[0]["valid_until"], Value::Null);
    assert_eq!(metadata[0]["source"], "synthetic-public-intake");
    let prior_context = f.read("recall", json!({}));

    let corrected = f.accept("corrected", "仅允许重新评审", &fact_key, Some(1));
    assert_eq!(corrected["record_id"], accepted["record_id"]);
    assert_eq!(
        f.store
            .read_current_projection(NS, &directory)
            .unwrap_err()
            .code,
        "projection_stale"
    );
    assert_eq!(
        f.read(
            "validate_context",
            json!({"manifest":prior_context["manifest"]})
        )["valid"],
        false
    );
    f.store.project(NS, &directory).unwrap();
    let refreshed = f.store.read_current_projection(NS, &directory).unwrap();
    assert_ne!(
        refreshed["manifest"]["memory_sha256"],
        first["manifest"]["memory_sha256"]
    );
    assert_eq!(refreshed["manifest"]["records"][0]["revision"], 2);
    assert_eq!(
        metadata_from_markdown(refreshed["body"].as_str().unwrap())[0]["fact_key"],
        fact_key
    );
    assert!(!refreshed["body"].as_str().unwrap().contains("允许部署"));
    f.read(
        "retract",
        json!({
            "id":accepted["record_id"],"expected_revision":2,
            "idempotency_key":"withdraw"
        }),
    );
    f.store.project(NS, &directory).unwrap();
    let withdrawn = f.store.read_current_projection(NS, &directory).unwrap();
    assert!(metadata_from_markdown(withdrawn["body"].as_str().unwrap()).is_empty());
    assert!(!withdrawn["body"].as_str().unwrap().contains("仅测试环境"));
}

#[test]
fn future_facts_retain_time_limits_only_in_history_and_are_not_projected_as_current() {
    let mut f = Fixture::new();
    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let pending = f.read(
        "candidate_submit",
        json!({
            "event_id":"future","body":"Future permission.","origin":"user_explicit",
            "source":"future-fixture","fact_key":key(),
            "valid_from":start,"valid_until":start+3600,"idempotency_key":"future"
        }),
    );
    let accepted = f.read(
        "candidate_resolve",
        json!({
            "candidate_id":pending["candidate_id"],"decision":"accept",
            "idempotency_key":"accept-future"
        }),
    );
    let id = &accepted["record_id"];
    let historical = f.read("inspect", json!({"id":id,"history":true}));
    assert_eq!(historical["fact_key"], key());
    assert_eq!(historical["valid_from"], start);
    assert_eq!(historical["valid_until"], start + 3600);
    assert_eq!(historical["applicable_now"], false);
    assert!(f.store.execute(NS, "inspect", json!({"id":id})).is_err());
    assert_eq!(f.read("recall", json!({}))["items"], json!([]));
    let directory = f.directory.path().join("future-projection");
    f.store.project(NS, &directory).unwrap();
    assert!(!read_projection(&directory).unwrap()["body"]
        .as_str()
        .unwrap()
        .contains("Future permission"));
}

#[test]
fn metadata_reads_stay_bound_to_namespace_and_storage_identity() {
    let mut f = Fixture::new();
    let accepted = f.accept("scope", "Scoped permission.", &key(), None);
    let pinned = f.read("binding", json!({}));
    let other = f.store.execute("other", "recall", json!({})).unwrap();
    assert_eq!(other["items"], json!([]));
    assert_eq!(
        f.store
            .execute("other", "inspect", json!({"id":accepted["record_id"]}))
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        f.store
            .execute_bound("other", "recall", json!({}), Some(&pinned))
            .unwrap_err()
            .code,
        "binding_mismatch"
    );
    let other_directory = tempfile::tempdir().unwrap();
    let mut other_store = Store::open(other_directory.path().join("other.db")).unwrap();
    assert_eq!(
        other_store
            .execute_bound(NS, "recall", json!({}), Some(&pinned))
            .unwrap_err()
            .code,
        "binding_mismatch"
    );
}

#[test]
fn editing_only_a_projection_condition_is_detected_before_it_can_be_served() {
    let mut f = Fixture::new();
    let mut fact_key = key();
    fact_key["conditions"] = json!("Requires ```written``` approval with \"quoted\" evidence");
    f.accept("condition", "Allowed conditionally.", &fact_key, None);
    let directory = f.directory.path().join("tamper-projection");
    f.store.project(NS, &directory).unwrap();
    let original = read_projection(&directory).unwrap();
    assert_eq!(
        metadata_from_markdown(original["body"].as_str().unwrap())[0]["fact_key"],
        fact_key
    );
    let pointer: Value =
        serde_json::from_slice(&fs::read(directory.join("CURRENT")).unwrap()).unwrap();
    let path = directory
        .join(pointer["generation"].as_str().unwrap())
        .join("MEMORY.md");
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("Requires", "Ignores");
    fs::write(&path, edited).unwrap();
    assert_eq!(
        read_projection(&directory).unwrap_err().code,
        "projection_modified"
    );
    assert_eq!(
        f.store.project(NS, &directory).unwrap_err().code,
        "projection_modified"
    );
}
