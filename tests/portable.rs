//! Synthetic, local black-box migration contracts. No model or host configuration.
use recall_ledger::Store;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

struct Fixture {
    _dir: TempDir,
    path: std::path::PathBuf,
    ns: String,
    store: Store,
}

impl Fixture {
    fn new(ns: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let store = Store::open(&path).unwrap();
        Self {
            _dir: dir,
            path,
            ns: ns.into(),
            store,
        }
    }
    fn call(&mut self, method: &str, args: Value) -> Value {
        self.store
            .execute(&self.ns, method, args)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }
    fn write(&mut self, method: &str, mut args: Value) -> Value {
        args["idempotency_key"] = json!(Uuid::new_v4().to_string());
        self.call(method, args)
    }
    fn error(&mut self, method: &str, args: Value) -> String {
        self.store.execute(&self.ns, method, args).unwrap_err().code
    }
    fn remember(&mut self, text: &str) -> Value {
        self.write("remember", json!({"body":text,"source":"synthetic"}))
    }
    fn export(&mut self) -> Value {
        self.call("bundle_export", json!({}))
    }
    fn plan(&mut self, bundle: &Value) -> Value {
        self.call(
            "bundle_plan",
            json!({"bundle":bundle,"expected_source":bundle["payload"]["source_binding"]}),
        )
    }
    fn apply_args(&mut self, bundle: &Value) -> Value {
        let plan = self.plan(bundle);
        json!({
            "bundle":bundle,"expected_source":bundle["payload"]["source_binding"],
            "reviewed_digest":plan["reviewed_digest"],"expected_target_commit":plan["target_commit"],
            "idempotency_key":Uuid::new_v4().to_string()
        })
    }
    fn import(&mut self, bundle: &Value) -> Value {
        let args = self.apply_args(bundle);
        self.call("bundle_apply", args)
    }
    fn inspect(&mut self, id: &Value) -> Value {
        self.call("inspect", json!({"id":id}))
    }
    fn revise(&mut self, record: &Value, text: &str) -> Value {
        self.write(
            "revise",
            json!({"id":record["id"],"expected_revision":record["revision"],"body":text}),
        )
    }
    fn retract(&mut self, record: &Value) {
        self.write(
            "retract",
            json!({"id":record["id"],"expected_revision":record["revision"]}),
        );
    }
    fn derive(&mut self, parent: &Value, text: &str) -> Value {
        let job = self.write(
            "prepare",
            json!({"inputs":[parent["id"]],"processor":"synthetic-summary"}),
        );
        self.write("publish", json!({"job_id":job["job_id"],"body":text}))
    }
    fn submit(&mut self, event: &str, text: &str, key: Option<Value>) -> Value {
        let mut args =
            json!({"event_id":event,"body":text,"source":"synthetic","origin":"user_message"});
        if let Some(key) = key {
            args["fact_key"] = key;
        }
        self.write("candidate_submit", args)
    }
    fn accept(&mut self, candidate: &Value) -> Value {
        self.write(
            "candidate_resolve",
            json!({"candidate_id":candidate["candidate_id"],"decision":"accept"}),
        )
    }
}

fn fact_key() -> Value {
    json!({"entity":"alice","attribute":"drink","scope":"project","conditions":"morning"})
}

fn mapped(receipt: &Value, source_id: &Value, kind: &str) -> Value {
    receipt["mappings"][kind]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source_id"] == *source_id)
        .unwrap()["id"]
        .clone()
}

fn rehash(bundle: &mut Value) {
    bundle["digest"] = json!(format!(
        "{:x}",
        Sha256::digest(bundle["payload"].to_string().as_bytes())
    ));
}

fn no_bodies(report: &Value) {
    fn visit(v: &Value) {
        match v {
            Value::Object(o) => {
                assert!(!o.contains_key("body"));
                for child in o.values() {
                    visit(child);
                }
            }
            Value::Array(a) => {
                for child in a {
                    visit(child);
                }
            }
            _ => {}
        }
    }
    visit(report);
}

#[test]
fn complete_namespace_snapshot_preserves_admission_identity_and_dependencies() {
    let mut a = Fixture::new("source");
    let accepted = a.submit("accepted", "tea", Some(fact_key()));
    let accepted = a.accept(&accepted);
    let pending = a.submit("pending", "UNREVIEWED-SECRET", None);
    let parent = a.inspect(&accepted["record_id"]);
    let summary = a.derive(&parent, "tea summary");
    a.store
        .execute(
            "other",
            "remember",
            json!({"body":"other namespace","idempotency_key":"other"}),
        )
        .unwrap();
    let bundle = a.export();
    assert_eq!(bundle["payload"]["records"].as_array().unwrap().len(), 2);
    assert!(!bundle.to_string().contains("other namespace"));
    assert_eq!(
        bundle["digest"],
        format!(
            "{:x}",
            Sha256::digest(bundle["payload"].to_string().as_bytes())
        )
    );
    let mut b = Fixture::new("target");
    let before = b.call("status", json!({}));
    let plan = b.plan(&bundle);
    assert_eq!(plan["can_apply"], true);
    no_bodies(&plan);
    assert_eq!(
        b.call("status", json!({}))["commit_token"],
        before["commit_token"]
    );
    assert_eq!(b.call("status", json!({}))["records"], 0);
    let receipt = b.import(&bundle);
    no_bodies(&receipt);
    let fact = b.call("fact_get", json!({"fact_key":fact_key()}));
    assert_eq!(fact["body"], "tea");
    assert_eq!(
        b.inspect(&mapped(&receipt, &summary["id"], "records"))["dependencies"][0]["id"],
        fact["id"]
    );
    let candidate = b.call(
        "candidate_inspect",
        json!({"candidate_id":mapped(&receipt,&pending["candidate_id"],"candidates")}),
    );
    assert_eq!(candidate["state"], "pending");
    assert_eq!(candidate["body"], "UNREVIEWED-SECRET");
    assert!(!b
        .call("recall", json!({}))
        .to_string()
        .contains("UNREVIEWED-SECRET"));
    let roundtrip = b.export();
    assert_eq!(
        roundtrip["payload"]["records"][0]["identity"],
        bundle["payload"]["records"][0]["identity"]
    );
}

#[test]
fn digest_schema_and_source_binding_are_checked_before_plan_or_write() {
    let mut a = Fixture::new("source");
    a.remember("original");
    let bundle = a.export();
    let mut b = Fixture::new("target");
    let mut bad = bundle.clone();
    bad["payload"]["records"][0]["body"] = json!("tampered");
    assert_eq!(
        b.error(
            "bundle_plan",
            json!({"bundle":bad,"expected_source":bundle["payload"]["source_binding"]})
        ),
        "digest_mismatch"
    );
    for mutate in [
        |v: &mut Value| {
            v["format"] = json!("unknown");
        },
        |v: &mut Value| {
            v["payload"]["invented"] = json!(true);
        },
        |v: &mut Value| {
            v["payload"]["records"][0]
                .as_object_mut()
                .unwrap()
                .remove("base_hash");
        },
        |v: &mut Value| {
            v["payload"]["records"][0]["status"] = json!("retracted");
        },
        |v: &mut Value| {
            v["payload"]["records"][0]["revision"] = json!(0);
        },
    ] {
        let mut bad = bundle.clone();
        mutate(&mut bad);
        rehash(&mut bad);
        assert_eq!(
            b.error(
                "bundle_plan",
                json!({"bundle":bad,"expected_source":bundle["payload"]["source_binding"]})
            ),
            "bundle_invalid"
        );
    }
    let other = b.call("binding", json!({}));
    assert_eq!(
        b.error(
            "bundle_plan",
            json!({"bundle":bundle,"expected_source":other})
        ),
        "binding_mismatch"
    );
    assert_eq!(b.call("status", json!({}))["records"], 0);
}

#[test]
fn reviewed_digest_and_exact_target_commit_are_required() {
    let mut a = Fixture::new("a");
    a.remember("one");
    let bundle = a.export();
    let mut b = Fixture::new("b");
    let mut args = b.apply_args(&bundle);
    args["reviewed_digest"] = json!("0".repeat(64));
    assert_eq!(b.error("bundle_apply", args), "digest_mismatch");
    let args = b.apply_args(&bundle);
    b.remember("unrelated target write");
    assert_eq!(b.error("bundle_apply", args), "target_changed");
    assert_eq!(b.call("status", json!({}))["records"], 1);
}

#[test]
fn repeated_import_is_durable_and_source_events_stay_deduplicated() {
    let mut a = Fixture::new("a");
    let proposal = a.submit("event", "pending", None);
    a.remember("one");
    let bundle = a.export();
    let mut b = Fixture::new("b");
    let args = b.apply_args(&bundle);
    let first = b.call("bundle_apply", args.clone());
    b.store = Store::open(&b.path).unwrap();
    assert_eq!(b.call("bundle_apply", args), first);
    let second = b.import(&bundle);
    assert_eq!(second["replayed"], true);
    assert_eq!(b.call("status", json!({}))["records"], 1);
    let retry = b.submit("event", "pending", None);
    assert_eq!(
        retry["candidate_id"],
        mapped(&first, &proposal["candidate_id"], "candidates")
    );
    assert_eq!(b.call("candidate_list", json!({}))["total"], 1);
}

#[test]
fn source_revision_updates_same_target_record_and_invalidates_local_artifacts_and_vector() {
    let mut a = Fixture::new("a");
    let original = a.remember("one");
    let bundle = a.export();
    let mut b = Fixture::new("b");
    let first = b.import(&bundle);
    let target_id = mapped(&first, &original["id"], "records");
    let target = b.inspect(&target_id);
    let summary = b.derive(&target, "local derived one");
    b.write(
        "embedding_set",
        json!({"id":target_id,"expected_revision":1,"model_key":"synthetic/1","vector":[1.0,0.0]}),
    );
    a.revise(&original, "two");
    let updated = a.export();
    let receipt = b.import(&updated);
    assert_eq!(mapped(&receipt, &original["id"], "records"), target_id);
    assert_eq!(b.inspect(&target_id)["body"], "two");
    assert_eq!(b.inspect(&target_id)["revision"], 2);
    assert_eq!(
        b.error("inspect", json!({"id":summary["id"]})),
        "stale_dependency"
    );
    let vector = b.call(
        "recall",
        json!({"mode":"vector","model_key":"synthetic/1","query_vector":[1.0,0.0]}),
    );
    assert!(vector["items"].as_array().unwrap().is_empty());
}

#[test]
fn withdrawal_erases_target_history_candidates_and_extra_descendants_and_old_snapshots_cannot_revive(
) {
    let mut a = Fixture::new("a");
    let proposal = a.submit("accepted", "WITHDRAW-ME", Some(fact_key()));
    let accepted = a.accept(&proposal);
    let original = a.inspect(&accepted["record_id"]);
    let old = a.export();
    let mut b = Fixture::new("b");
    let imported = b.import(&old);
    let id = mapped(&imported, &original["id"], "records");
    let target = b.inspect(&id);
    let summary = b.derive(&target, "DERIVED-WITHDRAW");
    let extra = b.submit("target-pending", "LOCAL-PROPOSAL", Some(fact_key()));
    a.retract(&original);
    let tombstone = a.export();
    assert!(!tombstone.to_string().contains("WITHDRAW-ME"));
    b.import(&tombstone);
    for id in [id, summary["id"].clone()] {
        assert_eq!(
            b.error("inspect", json!({"id":id,"history":true})),
            "not_found"
        );
    }
    for id in [
        extra["candidate_id"].clone(),
        mapped(&imported, &proposal["candidate_id"], "candidates"),
    ] {
        let view = b.call("candidate_inspect", json!({"candidate_id":id}));
        assert_eq!(view["state"], "withdrawn");
        assert!(view["body"].is_null());
    }
    assert_eq!(b.plan(&old)["can_apply"], false);
    let args = b.apply_args(&old);
    assert_eq!(b.error("bundle_apply", args), "migration_conflict");
    let sql = rusqlite::Connection::open(&b.path).unwrap();
    let bodies: String = sql
        .query_row("SELECT group_concat(body) FROM revisions", [], |r| r.get(0))
        .unwrap();
    assert!(!bodies.contains("WITHDRAW") && !bodies.contains("LOCAL-PROPOSAL"));
}

#[test]
fn first_seen_tombstone_erases_existing_pending_proposal_for_same_fact_key() {
    let mut a = Fixture::new("a");
    let proposal = a.submit("source", "withdrawn fact", Some(fact_key()));
    let accepted = a.accept(&proposal);
    let original = a.inspect(&accepted["record_id"]);
    a.retract(&original);
    let bundle = a.export();
    let mut b = Fixture::new("b");
    let pending = b.submit("target", "PENDING-SECRET", Some(fact_key()));
    b.import(&bundle);
    let candidate = b.call(
        "candidate_inspect",
        json!({"candidate_id":pending["candidate_id"]}),
    );
    assert_eq!(candidate["state"], "withdrawn");
    assert!(candidate["body"].is_null());
    assert!(!b.export().to_string().contains("PENDING-SECRET"));
}

#[test]
fn exact_bundle_replay_after_local_withdrawal_is_a_noop_including_pending_and_derived_items() {
    let mut a = Fixture::new("a");
    let proposal = a.submit("accepted", "original", Some(fact_key()));
    let accepted = a.accept(&proposal);
    let root = a.inspect(&accepted["record_id"]);
    a.derive(&root, "derived");
    a.submit("pending", "unreviewed", Some(fact_key()));
    let bundle = a.export();
    let mut b = Fixture::new("b");
    let receipt = b.import(&bundle);
    let target = b.inspect(&mapped(&receipt, &root["id"], "records"));
    b.retract(&target);
    let replay = b.import(&bundle);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["stats"]["records"]["update"], 0);
    assert_eq!(replay["stats"]["candidates"]["update"], 0);
    assert!(b.call("recall", json!({}))["items"]
        .as_array()
        .unwrap()
        .is_empty());
    let state = b.export();
    assert!(state["payload"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["status"] == "retracted" && r["body"].is_null()));
    assert!(state["payload"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["state"] == "withdrawn" && c["body"].is_null()));
    assert_eq!(b.error("bundle_context",json!({
        "bundle":bundle,"expected_source":bundle["payload"]["source_binding"],
        "manifest":{"schema_version":"recall-ledger.context/1","namespace":"a",
            "commit_token":root["commit_token"],"records":[{"id":root["id"],"revision":root["revision"]}]}
    })),"stale_dependency");
}

#[test]
fn terminal_candidates_and_same_revision_records_cannot_be_rewritten_by_newer_envelope() {
    let mut a = Fixture::new("a");
    let proposal = a.submit("event", "proposal", None);
    a.write(
        "candidate_resolve",
        json!({"candidate_id":proposal["candidate_id"],"decision":"reject"}),
    );
    a.remember("original");
    let bundle = a.export();
    let mut b = Fixture::new("b");
    b.import(&bundle);
    let mut newer = bundle.clone();
    newer["payload"]["source_sequence"] = json!(999);
    newer["payload"]["candidates"][0]["state"] = json!("pending");
    newer["payload"]["candidates"][0]["body"] = json!("proposal");
    rehash(&mut newer);
    assert_eq!(b.plan(&newer)["can_apply"], false);
    let mut newer = bundle.clone();
    newer["payload"]["source_sequence"] = json!(999);
    newer["payload"]["records"][0]["body"] = json!("same revision rewritten");
    rehash(&mut newer);
    let plan = b.plan(&newer);
    assert_eq!(plan["can_apply"], false);
    assert!(plan["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["reason"] == "revision_collision"));
}

#[test]
fn destination_local_withdrawal_and_concurrent_edits_are_not_overwritten() {
    for retract in [false, true] {
        let mut a = Fixture::new("a");
        let original = a.remember("one");
        let mut b = Fixture::new("b");
        let receipt = b.import(&a.export());
        let target = b.inspect(&mapped(&receipt, &original["id"], "records"));
        if retract {
            b.retract(&target);
        } else {
            b.revise(&target, "local two");
        }
        let next = a.revise(&original, "remote two");
        a.revise(&next, "remote three");
        let incoming = a.export();
        assert_eq!(b.plan(&incoming)["can_apply"], false);
        let args = b.apply_args(&incoming);
        assert_eq!(b.error("bundle_apply", args), "migration_conflict");
        if retract {
            assert_eq!(b.error("inspect", json!({"id":target["id"]})), "not_found");
        } else {
            assert_eq!(b.inspect(&target["id"])["body"], "local two");
        }
    }
}

#[test]
fn candidate_acceptance_can_follow_reviewed_source_but_never_be_promoted_by_transfer_alone() {
    let mut a = Fixture::new("a");
    let proposed = a.submit("event", "proposal", Some(fact_key()));
    let pending = a.export();
    let mut b = Fixture::new("b");
    let first = b.import(&pending);
    assert!(b.call("recall", json!({}))["items"]
        .as_array()
        .unwrap()
        .is_empty());
    a.accept(&proposed);
    let accepted = a.export();
    let receipt = b.import(&accepted);
    assert_eq!(receipt["stats"]["records"]["create"], 1);
    let candidate = b.call(
        "candidate_inspect",
        json!({"candidate_id":mapped(&first,&proposed["candidate_id"],"candidates")}),
    );
    assert_eq!(candidate["state"], "accepted");
    assert_eq!(candidate["body"], "proposal");
    assert_eq!(b.plan(&pending)["can_apply"], false);
}

#[test]
fn a_to_b_to_a_preserves_root_identity_and_applies_a_descendant_edit_once() {
    let mut a = Fixture::new("a");
    let root = a.remember("version one");
    let child = a.derive(&root, "initial summary");
    let original = a.export();
    let mut b = Fixture::new("b");
    let imported_b = b.import(&original);
    let b_root = b.inspect(&mapped(&imported_b, &root["id"], "records"));
    b.revise(&b_root, "version two from B");
    let from_b = b.export();
    let result = a.import(&from_b);
    assert_eq!(result["stats"]["records"]["create"], 0);
    assert_eq!(a.inspect(&root["id"])["body"], "version two from B");
    assert_eq!(
        a.error("inspect", json!({"id":child["id"]})),
        "stale_dependency"
    );
    assert_eq!(a.call("status", json!({}))["records"], 2);
    b.import(&a.export());
    assert_eq!(b.call("status", json!({}))["records"], 2);
}

#[test]
fn unedited_multihop_cycles_keep_identity_but_unprovable_edit_ancestry_fails_closed() {
    let mut a = Fixture::new("a");
    a.remember("one");
    let original = a.export();
    let mut b = Fixture::new("b");
    b.import(&original);
    let mut c = Fixture::new("c");
    c.import(&b.export());
    let unchanged = a.import(&c.export());
    assert_eq!(unchanged["stats"]["records"]["create"], 0);
    assert_eq!(a.call("status", json!({}))["records"], 1);
    // A distinct native source has never established an import baseline with an
    // intermediate; C's last-imported hash cannot prove A's old edit ancestry.
    let mut a = Fixture::new("native");
    let root = a.remember("one");
    let mut b = Fixture::new("bridge");
    let receipt_b = b.import(&a.export());
    let current_b = b.inspect(&mapped(&receipt_b, &root["id"], "records"));
    b.revise(&current_b, "two");
    let mut c = Fixture::new("last-hop");
    c.import(&b.export());
    let edited = c.export();
    assert_eq!(a.plan(&edited)["can_apply"], false);
    assert_eq!(a.inspect(&root["id"])["body"], "one");
}

#[test]
fn independent_branch_with_higher_revision_cannot_use_another_sources_baseline() {
    let mut a = Fixture::new("root");
    let root = a.remember("original");
    let snapshot = a.export();
    let mut b = Fixture::new("branch-b");
    let rb = b.import(&snapshot);
    let mut c = Fixture::new("branch-c");
    let rc = c.import(&snapshot);
    // Establish a C-specific previous snapshot as well: it still cannot use B's
    // later baseline as permission to overwrite a state that C has never seen.
    a.import(&c.export());
    let b_record = b.inspect(&mapped(&rb, &root["id"], "records"));
    b.revise(&b_record, "B selected value");
    a.import(&b.export());
    let c_record = c.inspect(&mapped(&rc, &root["id"], "records"));
    let c_two = c.revise(&c_record, "C alternative");
    c.revise(&c_two, "C revision three");
    let incoming = c.export();
    let plan = a.plan(&incoming);
    assert_eq!(plan["can_apply"], false);
    assert!(plan["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["reason"] == "concurrent_edit"));
    let args = a.apply_args(&incoming);
    assert_eq!(a.error("bundle_apply", args), "migration_conflict");
    assert_eq!(a.inspect(&root["id"])["body"], "B selected value");
}

#[test]
fn exact_fact_and_source_event_collisions_abort_whole_import() {
    for candidate_collision in [false, true] {
        let mut a = Fixture::new("a");
        let mut b = Fixture::new("b");
        a.remember("must roll back");
        if candidate_collision {
            a.submit("same-event", "first", None);
            b.submit("same-event", "second", None);
        } else {
            let p = a.submit("a-event", "tea", Some(fact_key()));
            a.accept(&p);
            let p = b.submit("b-event", "coffee", Some(fact_key()));
            b.accept(&p);
        }
        let before = b.export();
        let incoming = a.export();
        assert_eq!(b.plan(&incoming)["can_apply"], false);
        let args = b.apply_args(&incoming);
        assert_eq!(b.error("bundle_apply", args), "migration_conflict");
        assert_eq!(b.export(), before);
    }
}

#[test]
fn newer_full_snapshot_cannot_silently_omit_known_objects_or_reuse_sequence() {
    let mut a = Fixture::new("a");
    a.remember("one");
    a.remember("two");
    let original = a.export();
    let mut b = Fixture::new("b");
    b.import(&original);
    let mut omitted = original.clone();
    omitted["payload"]["records"].as_array_mut().unwrap().pop();
    omitted["payload"]["source_sequence"] = json!(999);
    rehash(&mut omitted);
    assert_eq!(
        b.plan(&omitted)["conflicts"][0]["reason"],
        "missing_tombstones"
    );
    let mut collision = original.clone();
    collision["payload"]["records"][0]["body"] = json!("forged same version");
    rehash(&mut collision);
    assert_eq!(
        b.plan(&collision)["conflicts"][0]["reason"],
        "source_sequence_collision"
    );
}

#[test]
fn invalid_graphs_and_nonpending_body_admission_are_rejected_atomically() {
    let mut a = Fixture::new("a");
    a.remember("root");
    a.submit("event", "pending", None);
    let original = a.export();
    let mut b = Fixture::new("b");
    let mut cycle = original.clone();
    cycle["payload"]["records"][0]["dependencies"] = json!([{
        "identity":cycle["payload"]["records"][0]["identity"],"revision":1
    }]);
    rehash(&mut cycle);
    assert_eq!(
        b.error(
            "bundle_plan",
            json!({"bundle":cycle,"expected_source":original["payload"]["source_binding"]})
        ),
        "bundle_invalid"
    );
    let mut accepted = original.clone();
    accepted["payload"]["candidates"][0]["state"] = json!("accepted");
    rehash(&mut accepted);
    assert_eq!(
        b.error(
            "bundle_plan",
            json!({"bundle":accepted,"expected_source":original["payload"]["source_binding"]})
        ),
        "bundle_invalid"
    );
    assert_eq!(b.call("status", json!({}))["records"], 0);
}

#[test]
fn bundle_context_rebinds_source_local_ids_only_after_import_and_revalidates_state() {
    let mut a = Fixture::new("a");
    let original = a.remember("context fact");
    let bundle = a.export();
    let manifest = json!({
        "schema_version":"recall-ledger.context/1","namespace":"a",
        "commit_token":original["commit_token"],"records":[{"id":original["id"],"revision":1}]
    });
    let args = json!({"bundle":bundle,"expected_source":bundle["payload"]["source_binding"],"manifest":manifest});
    let mut b = Fixture::new("b");
    assert_eq!(
        b.error("bundle_context", args.clone()),
        "migration_conflict"
    );
    let receipt = b.import(&bundle);
    let context = b.call("bundle_context", args.clone());
    assert_eq!(context["evidence_valid"], true);
    assert_eq!(
        context["manifest"]["records"][0]["id"],
        mapped(&receipt, &original["id"], "records")
    );
    no_bodies(&context);
    // Re-exporting B requires B's source_id even though the root identity is A's.
    let from_b = b.export();
    let mut c = Fixture::new("c");
    c.import(&from_b);
    let context_c = c.call("bundle_context",json!({
        "bundle":from_b,"expected_source":from_b["payload"]["source_binding"],"manifest":context["manifest"]
    }));
    assert_eq!(context_c["evidence_valid"], true);
    let target = b.inspect(&mapped(&receipt, &original["id"], "records"));
    b.revise(&target, "changed after handoff");
    assert_eq!(b.error("bundle_context", args), "stale_dependency");
}

#[test]
fn future_and_expired_records_travel_but_do_not_become_current_evidence() {
    let mut a = Fixture::new("a");
    let future = a.write("remember", json!({"body":"future","valid_from":i64::MAX-1}));
    a.write("remember", json!({"body":"expired","valid_until":1}));
    let bundle = a.export();
    let mut b = Fixture::new("b");
    b.import(&bundle);
    assert!(b.call("recall", json!({}))["items"]
        .as_array()
        .unwrap()
        .is_empty());
    let manifest = json!({
        "schema_version":"recall-ledger.context/1","namespace":"a",
        "commit_token":future["commit_token"],"records":[{"id":future["id"],"revision":1}]
    });
    assert_eq!(b.error("bundle_context",json!({"bundle":bundle,"expected_source":bundle["payload"]["source_binding"],"manifest":manifest})),"stale_dependency");
}

#[test]
fn export_is_current_only_and_excludes_vectors_jobs_receipts_and_old_revision_text() {
    let mut a = Fixture::new("a");
    let original = a.remember("HISTORICAL-SECRET");
    let next = a.revise(&original, "current");
    a.write("embedding_set",json!({"id":next["id"],"expected_revision":2,"model_key":"VECTOR-MODEL-SECRET","vector":[1.0,0.0]}));
    a.write(
        "prepare",
        json!({"inputs":[next["id"]],"processor":"LEASE-PROCESSOR-SECRET"}),
    );
    let bundle = a.export();
    let text = bundle.to_string();
    for forbidden in [
        "HISTORICAL-SECRET",
        "VECTOR-MODEL-SECRET",
        "LEASE-PROCESSOR-SECRET",
        "idempotency_key",
        "receipt",
        "job_id",
    ] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    let mut b = Fixture::new("b");
    let receipt = b.import(&bundle);
    let history = b.call(
        "inspect",
        json!({"id":mapped(&receipt,&original["id"],"records"),"history":true}),
    );
    assert_eq!(history["history"].as_array().unwrap().len(), 1);
    assert_eq!(history["history"][0]["revision"], 2);
}

#[test]
fn export_refuses_over_limit_payload_instead_of_truncating() {
    let mut a = Fixture::new("a");
    for _ in 0..9 {
        a.remember(&"x".repeat(60_000));
    }
    assert_eq!(a.error("bundle_export", json!({})), "resource_exhausted");
}

#[test]
fn restoring_an_old_backup_keeps_root_identity_and_cannot_bypass_known_withdrawal() {
    let mut a = Fixture::new("source");
    // Deliberately no fact key: tombstone protection must use root identity.
    let root = a.remember("RETRACTED-BEFORE-RESTORE");
    let candidate = a.submit("pending-event", "pending proposal", None);
    let old = a.export();
    let backup = a._dir.path().join("old-backup.db");
    a.store.backup(&backup).unwrap();
    let mut b = Fixture::new("target");
    let original_import = b.import(&old);
    a.retract(&root);
    b.import(&a.export());
    let restored_path = a._dir.path().join("restored.db");
    Store::restore(&backup, &restored_path).unwrap();
    let mut restored = Store::open(&restored_path).unwrap();
    let snapshot = restored
        .execute("source", "bundle_export", json!({}))
        .unwrap();
    assert_ne!(
        snapshot["payload"]["source_binding"],
        old["payload"]["source_binding"]
    );
    assert_eq!(
        snapshot["payload"]["records"][0]["identity"],
        old["payload"]["records"][0]["identity"]
    );
    assert_eq!(snapshot["payload"]["records"][0]["base_hash"], Value::Null);
    assert_eq!(
        snapshot["payload"]["candidates"][0]["identity"],
        old["payload"]["candidates"][0]["identity"]
    );
    assert_eq!(
        snapshot["payload"]["candidates"][0]["source_id"],
        candidate["candidate_id"]
    );
    assert_eq!(b.plan(&snapshot)["can_apply"], false);
    let args = b.apply_args(&snapshot);
    assert_eq!(b.error("bundle_apply", args), "migration_conflict");
    assert_eq!(
        b.error(
            "inspect",
            json!({"id":mapped(&original_import,&root["id"],"records")})
        ),
        "not_found"
    );
    assert!(b.call("recall", json!({}))["items"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn restore_pins_native_origins_across_namespaces_without_portable_object_limit() {
    let mut a = Fixture::new("large");
    for _ in 0..1001 {
        a.remember("synthetic");
    }
    a.store.execute("other","candidate_submit",json!({
        "event_id":"other","body":"pending","origin":"import","source":"fixture","idempotency_key":"other"
    })).unwrap();
    let before = a
        .store
        .execute("other", "bundle_export", json!({}))
        .unwrap();
    assert_eq!(a.error("bundle_export", json!({})), "resource_exhausted");
    let backup = a._dir.path().join("large-backup.db");
    let destination = a._dir.path().join("large-restored.db");
    a.store.backup(&backup).unwrap();
    Store::restore(&backup, &destination).unwrap();
    let mut restored = Store::open(&destination).unwrap();
    let after = restored
        .execute("other", "bundle_export", json!({}))
        .unwrap();
    assert_eq!(
        after["payload"]["candidates"][0]["identity"],
        before["payload"]["candidates"][0]["identity"]
    );
    assert_ne!(
        after["payload"]["source_binding"],
        before["payload"]["source_binding"]
    );
    let sql = rusqlite::Connection::open(&destination).unwrap();
    let count: i64 = sql
        .query_row(
            "SELECT count(*) FROM portable_origins WHERE baseline_hash IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1002);
}
