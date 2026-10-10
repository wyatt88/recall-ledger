//! Synthetic, black-box candidate intake and stable identity contract checks.
//! No model calls, host configuration or user memory are involved.
use recall_ledger::Store;
use serde_json::{json, Value};
use std::sync::{Arc, Barrier};
use tempfile::TempDir;
use uuid::Uuid;

const NS: &str = "ingestion-tests";

struct Fixture {
    _dir: TempDir,
    path: std::path::PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.sqlite3");
        let store = Store::open(&path).unwrap();
        Self {
            _dir: dir,
            path,
            store,
        }
    }
    fn read(&mut self, method: &str, args: Value) -> Value {
        self.store
            .execute(NS, method, args)
            .unwrap_or_else(|error| panic!("{method} failed: {error}"))
    }
    fn write(&mut self, method: &str, mut args: Value) -> Value {
        if args.get("idempotency_key").is_none() {
            args["idempotency_key"] = json!(Uuid::new_v4().to_string());
        }
        self.read(method, args)
    }
    fn submit(&mut self, event: &str, body: &str, key: Option<Value>) -> Value {
        let mut args = payload(event, body);
        if let Some(key) = key {
            args["fact_key"] = key;
        }
        self.write("candidate_submit", args)
    }
    fn accept(&mut self, candidate: &Value, revision: Option<i64>) -> Value {
        let mut args = json!({"candidate_id":candidate["candidate_id"],"decision":"accept"});
        if let Some(revision) = revision {
            args["expected_revision"] = json!(revision);
        }
        self.write("candidate_resolve", args)
    }
    fn error(&mut self, method: &str, mut args: Value) -> String {
        if ["candidate_submit", "candidate_resolve"].contains(&method)
            && args.get("idempotency_key").is_none()
        {
            args["idempotency_key"] = json!(Uuid::new_v4().to_string());
        }
        self.store.execute(NS, method, args).unwrap_err().code
    }
}

fn payload(event: &str, body: &str) -> Value {
    json!({"event_id":event,"body":body,"origin":"user_explicit","source":"test-source"})
}

fn identity(entity: &str, conditions: &str) -> Value {
    json!({
        "entity":entity,"attribute":"preferred_drink",
        "scope":"test-project","conditions":conditions
    })
}

fn resolve_args(candidate: &Value, expected: Option<i64>) -> Value {
    let mut args = json!({
        "candidate_id":candidate["candidate_id"],"decision":"accept",
        "idempotency_key":Uuid::new_v4().to_string()
    });
    if let Some(expected) = expected {
        args["expected_revision"] = json!(expected);
    }
    args
}

fn assert_no_body(value: &Value, body: &str) {
    assert!(
        !value.to_string().contains(body),
        "response exposed body: {value}"
    );
}

#[test]
fn every_origin_is_pending_until_an_explicit_acceptance() {
    let mut f = Fixture::new();
    for origin in [
        "user_explicit",
        "user_message",
        "tool_observation",
        "model_inference",
        "import",
    ] {
        let mut args = payload(origin, "proposal-secret");
        args["origin"] = json!(origin);
        let submitted = f.write("candidate_submit", args);
        assert_eq!(submitted["state"], "pending");
        assert_no_body(&submitted, "proposal-secret");
        let view = f.read(
            "candidate_inspect",
            json!({"candidate_id":submitted["candidate_id"]}),
        );
        assert_eq!(view["body"], "proposal-secret");
        assert_eq!(view["body_source"], "candidate_proposal");
        assert_eq!(view["trust_boundary"], "retrieved_data");
    }
    assert!(f.read("recall", json!({}))["items"]
        .as_array()
        .unwrap()
        .is_empty());
    let listing = f.read("candidate_list", json!({"state":"pending","limit":2}));
    assert_eq!(listing["total"], 5);
    assert_eq!(listing["truncated"], true);
    assert_eq!(listing["candidates"].as_array().unwrap().len(), 2);
    assert_no_body(&listing, "proposal-secret");
    assert_eq!(listing["candidates"][0]["body_included"], false);
}

#[test]
fn exact_fact_identity_does_not_merge_subjects_conditions_or_tuple_boundaries() {
    let mut f = Fixture::new();
    let keys = [
        identity("alice", "morning"),
        identity("bob", "morning"),
        identity("alice", "evening"),
        json!({"entity":"a|b","attribute":"c","scope":"x","conditions":"y"}),
        json!({"entity":"a","attribute":"b|c","scope":"x","conditions":"y"}),
        json!({"entity":"alice","attribute":"preferred_drink","scope":"other","conditions":"morning"}),
        json!({"entity":"alice","attribute":"avoided_drink","scope":"test-project","conditions":"morning"}),
        identity("Alice", "morning"),
    ];
    let mut ids = std::collections::HashSet::new();
    for (index, key) in keys.iter().enumerate() {
        let body = format!("separate-value-{index}");
        let submitted = f.submit(&format!("event-{index}"), &body, Some(key.clone()));
        let accepted = f.accept(&submitted, None);
        assert!(ids.insert(accepted["record_id"].as_str().unwrap().to_string()));
        let fact = f.read("fact_get", json!({"fact_key":key}));
        assert_eq!(fact["record_id"], accepted["record_id"]);
        assert_eq!(fact["revision"], 1);
        assert_eq!(fact["body"], body);
        assert_eq!(fact["fact_key"], *key);
    }
}

#[test]
fn acceptance_requires_current_revision_and_conflicts_leave_candidate_pending() {
    let mut f = Fixture::new();
    let key = identity("alice", "always");
    let first = f.submit("one", "tea", Some(key.clone()));
    let first = f.accept(&first, None);
    let second = f.submit("two", "coffee", Some(key.clone()));
    assert_eq!(
        f.error("candidate_resolve", resolve_args(&second, None)),
        "revision_conflict"
    );
    assert_eq!(
        f.error("candidate_resolve", resolve_args(&second, Some(999))),
        "revision_conflict"
    );
    assert_eq!(
        f.read(
            "candidate_inspect",
            json!({"candidate_id":second["candidate_id"]})
        )["state"],
        "pending"
    );
    assert_eq!(f.read("fact_get", json!({"fact_key":key}))["body"], "tea");
    let updated = f.accept(&second, Some(1));
    assert_eq!(updated["record_id"], first["record_id"]);
    assert_eq!(updated["revision"], 2);
    let current = f.read("fact_get", json!({"fact_key":key}));
    assert_eq!(current["body"], "coffee");
    assert_eq!(current["revision"], 2);
}

#[test]
fn two_concurrent_acceptances_cannot_both_overwrite_the_same_revision() {
    let mut f = Fixture::new();
    let key = identity("alice", "always");
    let seed = f.submit("seed", "initial", Some(key.clone()));
    f.accept(&seed, None);
    let first = f.submit("race-a", "alpha", Some(key.clone()));
    let second = f.submit("race-b", "beta", Some(key.clone()));
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [first, second]
        .into_iter()
        .map(|candidate| {
            let path = f.path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                barrier.wait();
                store.execute(NS, "candidate_resolve", resolve_args(&candidate, Some(1)))
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results.into_iter().find_map(Result::err).unwrap().code,
        "revision_conflict"
    );
    assert_eq!(
        f.read("candidate_list", json!({"state":"pending"}))["total"],
        1
    );
    assert_eq!(f.read("fact_get", json!({"fact_key":key}))["revision"], 2);
}

#[test]
fn event_identity_survives_restart_independent_of_operation_idempotency_key() {
    let mut f = Fixture::new();
    let mut args = payload("stable-event", "event-body-secret");
    args["fact_key"] = identity("alice", "always");
    args["idempotency_key"] = json!("first-submit");
    let original = f.write("candidate_submit", args.clone());
    f.store = Store::open(&f.path).unwrap();
    assert_eq!(f.write("candidate_submit", args.clone()), original);
    args["idempotency_key"] = json!("retry-with-new-operation-key");
    let replay = f.write("candidate_submit", args.clone());
    assert_eq!(replay["candidate_id"], original["candidate_id"]);
    assert_eq!(f.read("candidate_list", json!({}))["total"], 1);
    let recovered = f.read(
        "operation",
        json!({"method":"candidate_submit","idempotency_key":"first-submit"}),
    );
    assert_no_body(&recovered, "event-body-secret");
    args["body"] = json!("different body");
    args["idempotency_key"] = json!("conflicting-event");
    assert_eq!(f.error("candidate_submit", args), "event_conflict");
}

#[test]
fn namespace_isolation_applies_to_candidates_events_and_fact_bindings() {
    let mut f = Fixture::new();
    let key = identity("same-user", "always");
    let first = f.submit("same-event", "namespace-a-secret", Some(key.clone()));
    let accepted = f.accept(&first, None);
    let mut args = payload("same-event", "namespace-b-value");
    args["fact_key"] = key.clone();
    args["idempotency_key"] = json!("same-event");
    let other = f.store.execute("other", "candidate_submit", args).unwrap();
    let other_record = f
        .store
        .execute("other", "candidate_resolve", resolve_args(&other, None))
        .unwrap();
    assert_ne!(accepted["record_id"], other_record["record_id"]);
    let fact = f
        .store
        .execute("other", "fact_get", json!({"fact_key":key}))
        .unwrap();
    assert_eq!(fact["body"], "namespace-b-value");
    assert_no_body(&fact, "namespace-a-secret");
    for (method, args) in [
        (
            "candidate_inspect",
            json!({"candidate_id":first["candidate_id"]}),
        ),
        ("candidate_resolve", resolve_args(&first, None)),
    ] {
        assert_eq!(
            f.store.execute("other", method, args).unwrap_err().code,
            "not_found"
        );
    }
}

#[test]
fn source_retraction_withdraws_all_known_same_key_candidates_without_reimporting() {
    let mut f = Fixture::new();
    let key = identity("alice", "always");
    let mut original_args = payload("accepted-event", "accepted-private-body");
    original_args["fact_key"] = key.clone();
    original_args["idempotency_key"] = json!("original-submit");
    let first = f.write("candidate_submit", original_args.clone());
    let accepted = f.accept(&first, None);
    let pending = f.submit("pending-event", "pending-private-body", Some(key.clone()));
    let rejected = f.submit("rejected-event", "rejected-private-body", Some(key.clone()));
    f.write(
        "candidate_resolve",
        json!({"candidate_id":rejected["candidate_id"],"decision":"reject"}),
    );
    f.write(
        "retract",
        json!({"id":accepted["record_id"],"expected_revision":1}),
    );
    f.store = Store::open(&f.path).unwrap();
    for candidate in [&first, &pending, &rejected] {
        let view = f.read(
            "candidate_inspect",
            json!({"candidate_id":candidate["candidate_id"]}),
        );
        assert_eq!(view["state"], "withdrawn");
        assert_eq!(view["body_available"], false);
        for secret in [
            "accepted-private-body",
            "pending-private-body",
            "rejected-private-body",
        ] {
            assert_no_body(&view, secret);
        }
    }
    let listing = f.read("candidate_list", json!({"state":"withdrawn"}));
    assert_eq!(listing["total"], 3);
    assert_no_body(&listing, "private-body");
    assert_eq!(f.error("fact_get", json!({"fact_key":key})), "not_found");
    // The receipt is historical metadata; even its original pending state grants no body access.
    let receipt = f.write("candidate_submit", original_args.clone());
    assert_eq!(receipt["candidate_id"], first["candidate_id"]);
    assert_no_body(&receipt, "accepted-private-body");
    original_args["idempotency_key"] = json!("replayed-event-after-retraction");
    let replay = f.write("candidate_submit", original_args);
    assert_eq!(replay["state"], "withdrawn");
    assert_eq!(replay["candidate_id"], first["candidate_id"]);
    let mut fresh = payload("new-event", "must-not-resurrect");
    fresh["fact_key"] = key;
    assert_eq!(f.error("candidate_submit", fresh), "fact_retracted");
    assert_eq!(
        f.error("candidate_resolve", resolve_args(&pending, Some(1))),
        "candidate_state_conflict"
    );
}

#[test]
fn unkeyed_candidate_is_also_hidden_after_its_accepted_record_is_retracted() {
    let mut f = Fixture::new();
    let submitted = f.submit("unkeyed", "unkeyed-private-body", None);
    let accepted = f.accept(&submitted, None);
    f.write(
        "retract",
        json!({"id":accepted["record_id"],"expected_revision":1}),
    );
    let view = f.read(
        "candidate_inspect",
        json!({"candidate_id":submitted["candidate_id"]}),
    );
    assert_eq!(view["state"], "withdrawn");
    assert_no_body(&view, "unkeyed-private-body");
}

#[test]
fn accepted_candidate_reads_current_record_not_its_old_proposal() {
    let mut f = Fixture::new();
    let submitted = f.submit("current-view", "old-private-body", None);
    let accepted = f.accept(&submitted, None);
    f.write(
        "revise",
        json!({"id":accepted["record_id"],"expected_revision":1,"body":"new-current-body"}),
    );
    let view = f.read(
        "candidate_inspect",
        json!({"candidate_id":submitted["candidate_id"]}),
    );
    assert_no_body(&view, "old-private-body");
    assert_eq!(view["body"], "new-current-body");
    assert_eq!(view["body_source"], "current_record");
    assert_eq!(view["accepted_revision"], 1);
    assert_eq!(view["current_revision"], 2);
    assert_eq!(view["record_changed_since_acceptance"], true);
}

#[test]
fn rejected_candidate_is_terminal_and_never_creates_a_memory_record() {
    let mut f = Fixture::new();
    let submitted = f.submit("reject-event", "rejected-body-secret", None);
    let args = json!({
        "candidate_id":submitted["candidate_id"],"decision":"reject",
        "idempotency_key":"reject-once"
    });
    let rejected = f.write("candidate_resolve", args.clone());
    assert_eq!(f.write("candidate_resolve", args), rejected);
    assert_eq!(rejected["state"], "rejected");
    assert_no_body(&rejected, "rejected-body-secret");
    let view = f.read(
        "candidate_inspect",
        json!({"candidate_id":submitted["candidate_id"]}),
    );
    assert_eq!(view["body_available"], false);
    assert_no_body(&view, "rejected-body-secret");
    assert_eq!(
        f.error("candidate_resolve", resolve_args(&submitted, None)),
        "candidate_state_conflict"
    );
    assert_eq!(f.read("status", json!({}))["records"], 0);
}

#[test]
fn omitted_metadata_inherits_but_explicit_changes_cannot_silently_change_temporal_meaning() {
    let mut f = Fixture::new();
    let key = identity("alice", "defined interval");
    let mut args = payload("metadata-first", "first value");
    args["fact_key"] = key.clone();
    args["kind"] = json!("preference");
    args["volatility"] = json!("volatile");
    args["valid_from"] = json!(1);
    args["valid_until"] = json!(4_000_000_000_i64);
    let first = f.write("candidate_submit", args);
    let accepted = f.accept(&first, None);
    let mut args = payload("metadata-second", "second value");
    args["fact_key"] = key.clone();
    args["source"] = json!("new-observation-source");
    let second = f.write("candidate_submit", args);
    let revised = f.accept(&second, Some(1));
    assert_eq!(revised["record_id"], accepted["record_id"]);
    let fact = f.read("fact_get", json!({"fact_key":key}));
    assert_eq!(fact["kind"], "preference");
    assert_eq!(fact["volatility"], "volatile");
    assert_eq!(fact["valid_from"], 1);
    assert_eq!(fact["valid_until"], 4_000_000_000_i64);
    assert_eq!(fact["source"], "test-source");
    let view = f.read(
        "candidate_inspect",
        json!({"candidate_id":second["candidate_id"]}),
    );
    assert_eq!(view["source"], "new-observation-source");
    assert_eq!(view["current_record_metadata"]["source"], "test-source");
    assert_eq!(view["current_record_metadata"]["kind"], "preference");
    assert_eq!(view["current_record_metadata"]["volatility"], "volatile");
    for (field, value) in [
        ("kind", json!("assertion")),
        ("volatility", json!("stable")),
        ("valid_from", Value::Null),
        ("valid_until", json!(3_000_000_000_i64)),
    ] {
        let mut args = payload(&format!("metadata-conflict-{field}"), "conflicting value");
        args["fact_key"] = key.clone();
        args[field] = value;
        let candidate = f.write("candidate_submit", args);
        assert_eq!(
            f.error("candidate_resolve", resolve_args(&candidate, Some(2))),
            "fact_metadata_conflict"
        );
        assert_eq!(
            f.read(
                "candidate_inspect",
                json!({"candidate_id":candidate["candidate_id"]})
            )["state"],
            "pending"
        );
    }
    assert_eq!(f.read("fact_get", json!({"fact_key":key}))["revision"], 2);
}

#[test]
fn text_dates_negation_and_future_validity_are_not_inferred_or_rewritten() {
    let mut f = Fixture::new();
    let key = identity("alice", "historical narrative");
    let text = "I did not approve this on 2020-01-01; maybe I will in 2099.";
    let submitted = f.submit("negative", text, Some(key.clone()));
    let accepted = f.accept(&submitted, None);
    let current = f.read("fact_get", json!({"fact_key":key}));
    assert_eq!(current["body"], text);
    assert_eq!(current["valid_from"], Value::Null);
    assert_eq!(current["valid_until"], Value::Null);
    let mut args = payload("future", "future-body");
    args["fact_key"] = identity("alice", "future interval");
    args["valid_from"] = json!(4_000_000_000_i64);
    args["valid_until"] = json!(4_000_000_100_i64);
    let future = f.write("candidate_submit", args);
    f.accept(&future, None);
    let view = f.read(
        "candidate_inspect",
        json!({"candidate_id":future["candidate_id"]}),
    );
    assert_eq!(view["body_available"], false);
    assert_eq!(
        view["body_unavailable_reason"],
        "record_not_currently_valid"
    );
    assert_no_body(&view, "future-body");
    let recalled = f.read("recall", json!({"limit":100}));
    assert_eq!(recalled["items"].as_array().unwrap().len(), 1);
    assert_eq!(recalled["items"][0]["id"], accepted["record_id"]);
}

#[test]
fn candidate_acceptance_reuses_vector_invalidation_and_managed_summary_repair_fences() {
    let mut f = Fixture::new();
    let key = identity("project", "release schedule");
    let submitted = f.submit("first-release", "release in May", Some(key.clone()));
    let accepted = f.accept(&submitted, None);
    let record_id = accepted["record_id"].clone();
    let recipe = f.write(
        "summary_create",
        json!({"inputs":[record_id],"processor":"deterministic-test","max_chars":1000}),
    );
    let claim = f.write("maintenance_claim", json!({"worker":"test"}));
    f.write(
        "maintenance_complete",
        json!({"task_id":claim["task_id"],"lease_token":claim["lease_token"],"body":"May summary"}),
    );
    for id in [record_id.clone(), recipe["output_id"].clone()] {
        f.write(
            "embedding_set",
            json!({"id":id,"expected_revision":1,"model_key":"synthetic","vector":[1.0,0.0]}),
        );
    }
    let second = f.submit("second-release", "release in June", Some(key.clone()));
    f.accept(&second, Some(1));
    assert_eq!(
        f.error("inspect", json!({"id":recipe["output_id"]})),
        "stale_dependency"
    );
    let vector = f.read(
        "recall",
        json!({"query":"release","mode":"vector","model_key":"synthetic","query_vector":[1.0,0.0]}),
    );
    assert!(vector["items"].as_array().unwrap().is_empty());
    let old_repair = f.write("maintenance_claim", json!({"worker":"test"}));
    assert_eq!(old_repair["dependencies"][0]["revision"], 2);
    let third = f.submit("third-release", "release in July", Some(key));
    f.accept(&third, Some(2));
    let error = f.store.execute(
        NS,
        "maintenance_complete",
        json!({"task_id":old_repair["task_id"],"lease_token":old_repair["lease_token"],
            "body":"late June summary","idempotency_key":"late-summary"}),
    );
    assert_eq!(error.unwrap_err().code, "lease_conflict");
    let current_repair = f.write("maintenance_claim", json!({"worker":"test"}));
    assert_eq!(current_repair["dependencies"][0]["revision"], 3);
    f.write(
        "maintenance_complete",
        json!({"task_id":current_repair["task_id"],"lease_token":current_repair["lease_token"],
            "body":"July summary"}),
    );
    assert_eq!(
        f.read("inspect", json!({"id":recipe["output_id"]}))["body"],
        "July summary"
    );
}

#[test]
fn all_candidate_input_fields_are_validated_before_storage() {
    let mut f = Fixture::new();
    let base = payload("validation", "valid body");
    let mut cases = Vec::new();
    for required in ["event_id", "body", "origin", "source"] {
        let mut args = base.clone();
        args.as_object_mut().unwrap().remove(required);
        cases.push(args);
    }
    for (field, value) in [
        ("event_id", json!("")),
        ("event_id", json!("   ")),
        ("event_id", json!("x".repeat(257))),
        ("event_id", json!("x\n")),
        ("event_id", json!(1)),
        ("body", Value::Null),
        ("body", json!(" \n ")),
        ("body", json!("x".repeat(65_537))),
        ("origin", json!("trusted")),
        ("origin", Value::Null),
        ("source", json!("")),
        ("source", json!("x".repeat(257))),
        ("source", json!("x\0")),
        ("source", json!([])),
        ("kind", json!("truth")),
        ("kind", Value::Null),
        ("volatility", json!("frequent")),
        ("volatility", json!(7)),
        ("valid_from", json!(-1)),
        ("valid_from", json!(0.5)),
        ("valid_from", json!("tomorrow")),
        ("valid_until", json!(-1)),
        ("valid_until", json!(u64::MAX)),
        ("namespace", json!("override")),
        ("idempotency_key", json!("")),
        ("fact_key", Value::Null),
        ("fact_key", json!([])),
        ("fact_key", json!({"entity":"alice"})),
        (
            "fact_key",
            json!({"entity":"alice","attribute":"x","scope":"x","conditions":"x","extra":"x"}),
        ),
        (
            "fact_key",
            json!({"entity":" ","attribute":"x","scope":"x","conditions":"x"}),
        ),
        (
            "fact_key",
            json!({"entity":"alice","attribute":1,"scope":"x","conditions":"x"}),
        ),
        (
            "fact_key",
            json!({"entity":"alice","attribute":"x","scope":"x\n","conditions":"x"}),
        ),
        (
            "fact_key",
            json!({"entity":"alice","attribute":"x","scope":"x","conditions":"x".repeat(257)}),
        ),
    ] {
        let mut args = base.clone();
        args[field] = value;
        cases.push(args);
    }
    for (start, end) in [(1, 1), (2, 1)] {
        let mut args = base.clone();
        args["valid_from"] = json!(start);
        args["valid_until"] = json!(end);
        cases.push(args);
    }
    for (index, args) in cases.into_iter().enumerate() {
        assert_eq!(
            f.error("candidate_submit", args),
            "invalid_request",
            "invalid submission case {index}"
        );
    }
    assert_eq!(f.read("candidate_list", json!({}))["total"], 0);
}

#[test]
fn resolution_and_read_arguments_are_strictly_validated() {
    let mut f = Fixture::new();
    let submitted = f.submit("validation-record", "body", None);
    let base = resolve_args(&submitted, None);
    for (field, value) in [
        ("candidate_id", json!("")),
        ("candidate_id", json!(true)),
        ("candidate_id", json!("x\n")),
        ("decision", json!("auto")),
        ("decision", Value::Null),
        ("expected_revision", json!(0)),
        ("expected_revision", json!(-1)),
        ("expected_revision", json!(1.5)),
        ("expected_revision", json!("1")),
        ("expected_revision", Value::Null),
        ("expected_revision", json!(u64::MAX)),
        ("extra", json!(true)),
    ] {
        let mut args = base.clone();
        args[field] = value;
        assert_eq!(f.error("candidate_resolve", args), "invalid_request");
    }
    assert_eq!(
        f.error("candidate_resolve", resolve_args(&submitted, Some(1))),
        "revision_conflict"
    );
    assert_eq!(
        f.error(
            "candidate_resolve",
            json!({"candidate_id":submitted["candidate_id"],"decision":"reject","expected_revision":1})
        ),
        "invalid_request"
    );
    for args in [
        json!({"state":"active"}),
        json!({"state":null}),
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"limit":1.5}),
        json!({"limit":"1"}),
        json!({"unknown":true}),
    ] {
        assert_eq!(f.error("candidate_list", args), "invalid_request");
    }
    for args in [
        json!({}),
        json!({"candidate_id":null}),
        json!({"candidate_id":" "}),
    ] {
        assert_eq!(f.error("candidate_inspect", args), "invalid_request");
    }
    for args in [
        json!({}),
        json!({"fact_key":"alice"}),
        json!({"fact_key":{"entity":"a","attribute":"b","scope":"c","conditions":""}}),
        json!({"fact_key":identity("a","b"),"body":"override"}),
    ] {
        assert_eq!(f.error("fact_get", args), "invalid_request");
    }
    assert_eq!(
        f.read("candidate_list", json!({"state":"pending"}))["total"],
        1
    );
}
