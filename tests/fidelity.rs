//! Local synthetic source-preservation and adversarial publication checks.
//! Equality concerns recorded assertions, never the external truth of assertions.
use recall_ledger::Store;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;

const PROCESSOR: &str = "recall-ledger.evidence/1";

struct Fixture {
    _dir: TempDir,
    path: std::path::PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let store = Store::open(&path).unwrap();
        Self {
            _dir: dir,
            path,
            store,
        }
    }
    fn read(&mut self, method: &str, args: Value) -> Value {
        self.store
            .execute("main", method, args)
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }
    fn write(&mut self, method: &str, mut args: Value) -> Value {
        args["idempotency_key"] = json!(Uuid::new_v4().to_string());
        self.read(method, args)
    }
    fn error(&mut self, method: &str, mut args: Value) -> String {
        if [
            "evidence_create",
            "summary_create",
            "maintenance_complete",
            "revise",
        ]
        .contains(&method)
        {
            args["idempotency_key"] = json!(Uuid::new_v4().to_string());
        }
        self.store.execute("main", method, args).unwrap_err().code
    }
    fn remember(&mut self, text: &str) -> Value {
        self.write("remember", json!({"body":text}))
    }
    fn accept_fact(
        &mut self,
        event: &str,
        text: &str,
        key: &Value,
        revision: Option<i64>,
    ) -> Value {
        let candidate = self.write(
            "candidate_submit",
            json!({"event_id":event,"body":text,"origin":"user_explicit",
                "source":"operator","fact_key":key}),
        );
        assert_eq!(candidate["state"], "pending");
        let mut args = json!({"candidate_id":candidate["candidate_id"],"decision":"accept"});
        if let Some(revision) = revision {
            args["expected_revision"] = json!(revision);
        }
        self.write("candidate_resolve", args)
    }
    fn derive(&mut self, inputs: &[Value], text: &str, kind: &str) -> Value {
        let job = self.write(
            "prepare",
            json!({"inputs":inputs,"processor":"arbitrary-derivation"}),
        );
        self.write(
            "publish",
            json!({"job_id":job["job_id"],"body":text,"kind":kind}),
        )
    }
    fn create(&mut self, inputs: &[Value]) -> Value {
        self.write("evidence_create", json!({"inputs":inputs}))
    }
    fn claim(&mut self) -> Value {
        self.write(
            "maintenance_claim",
            json!({"worker":"synthetic","processor":PROCESSOR}),
        )
    }
    fn job(&mut self, claim: &Value) -> Value {
        self.read("maintenance_job", lease(claim))
    }
    fn complete(&mut self, claim: &Value, text: &Value) -> Value {
        let mut args = lease(claim);
        args["body"] = text.clone();
        self.write("maintenance_complete", args)
    }
    fn finish(&mut self) -> Value {
        let claim = self.claim();
        assert_eq!(claim["claimed"], true);
        let job = self.job(&claim);
        self.complete(&claim, &job["evidence_body"])
    }
    fn audit(&mut self, id: &Value) -> Value {
        self.read("evidence_inspect", json!({"id":id}))
    }
}
fn lease(claim: &Value) -> Value {
    json!({"task_id":claim["task_id"],"lease_token":claim["lease_token"]})
}
fn no_body(receipt: &Value, source: &str) {
    assert!(!receipt.to_string().contains(source));
    assert!(receipt.get("body").is_none());
    assert!(receipt.get("evidence_body").is_none());
}

#[test]
fn fact_identity_conditions_are_preserved_in_source_assertions() {
    let mut f = Fixture::new();
    let key = json!({
        "entity":"deployment service","attribute":"permission",
        "scope":"release pipeline","conditions":"test only; never production"
    });
    let candidate = f.write(
        "candidate_submit",
        json!({"event_id":"conditional-permission","body":"Deployment permitted.",
            "origin":"user_explicit","source":"operator","fact_key":key}),
    );
    let accepted = f.write(
        "candidate_resolve",
        json!({"candidate_id":candidate["candidate_id"],"decision":"accept"}),
    );
    let sources = f.read(
        "evidence_sources",
        json!({"inputs":[accepted["record_id"]]}),
    );
    assert_eq!(sources["roots"][0]["fact_key"], key);
    let body: Value = serde_json::from_str(sources["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["source_assertions"][0]["fact_key"], key);
    assert_eq!(
        body["source_assertions"][0]["body"],
        "Deployment permitted."
    );
    let recipe = f.create(&[accepted["record_id"].clone()]);
    let claim = f.claim();
    let job = f.job(&claim);
    assert_eq!(job["inputs"][0]["fact_key"], key);
    assert_eq!(job["evidence_body"], sources["body"]);
    let done = f.complete(&claim, &job["evidence_body"]);
    no_body(&done, key["conditions"].as_str().unwrap());
    let audit = f.audit(&done["id"]);
    assert_eq!(audit["verified"], true);
    assert_eq!(audit["roots"][0]["fact_key"], key);
    assert_eq!(audit["body"], sources["body"]);

    let corrected = f.accept_fact(
        "conditional-permission-correction",
        "Deployment requires two reviewers.",
        &key,
        Some(1),
    );
    assert_eq!(corrected["record_id"], accepted["record_id"]);
    assert_eq!(corrected["revision"], 2);
    assert_eq!(
        f.error("evidence_inspect", json!({"id":done["id"]})),
        "stale_dependency"
    );
    let repaired = f.finish();
    assert_eq!(repaired["id"], recipe["output_id"]);
    assert_eq!(repaired["revision"], 2);
    let audit = f.audit(&repaired["id"]);
    assert_eq!(audit["verified"], true);
    assert_eq!(audit["roots"][0]["fact_key"], key);
    assert_eq!(audit["roots"][0]["revision"], 2);
    let body: Value = serde_json::from_str(audit["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["source_assertions"][0]["fact_key"], key);
    assert_eq!(
        body["source_assertions"][0]["body"],
        "Deployment requires two reviewers."
    );

    // A changed condition denotes another fact, not mutable metadata on this
    // record. It cannot quietly widen the condition on an existing capsule.
    let mut other_key = key.clone();
    other_key["conditions"] = json!("production only after explicit approval");
    let other = f.accept_fact(
        "production-permission",
        "Deployment denied.",
        &other_key,
        None,
    );
    assert_ne!(other["record_id"], accepted["record_id"]);
    assert_eq!(f.audit(&repaired["id"])["body"], audit["body"]);
    f.write(
        "retract",
        json!({"id":accepted["record_id"],"expected_revision":2}),
    );
    assert_eq!(
        f.error("evidence_inspect", json!({"id":repaired["id"]})),
        "not_found"
    );
    assert_eq!(
        f.read("evidence_sources", json!({"inputs":[other["record_id"]]}))["roots"][0]["fact_key"],
        other_key
    );
}

#[test]
fn structured_fact_identity_counts_toward_complete_character_and_utf8_budgets() {
    let mut f = Fixture::new();
    let key = json!({
        "entity":"部署服务".repeat(20),"attribute":"权限".repeat(40),
        "scope":"本地测试租户".repeat(14),"conditions":"仅测试，不适用生产。".repeat(8)
    });
    let fact = f.accept_fact("budget-fact", "Deployment permitted.", &key, None);
    let sources = f.read(
        "evidence_sources",
        json!({"inputs":[fact["record_id"]],"max_chars":65536}),
    );
    let text = sources["body"].as_str().unwrap();
    assert_eq!(sources["coverage"]["output_chars"], text.chars().count());
    assert_eq!(sources["coverage"]["output_utf8_bytes"], text.len());
    let mut without_identity: Value = serde_json::from_str(text).unwrap();
    without_identity["source_assertions"][0]
        .as_object_mut()
        .unwrap()
        .remove("fact_key");
    // This would fit the pre-fix representation while losing the only condition.
    let old_char_budget = without_identity.to_string().chars().count();
    assert!(old_char_budget < text.chars().count());
    for method in ["evidence_sources", "evidence_create", "summary_create"] {
        let mut args = json!({"inputs":[fact["record_id"]],"max_chars":old_char_budget});
        if method == "summary_create" {
            args["processor"] = json!(PROCESSOR);
        }
        assert_eq!(f.error(method, args), "resource_exhausted");
    }

    // The leaf body remains within its own limit, but metadata takes the whole
    // UTF-8 capsule beyond 64 KiB even though its character budget would fit.
    let text_overhead = text.len() - "Deployment permitted.".len();
    let body_len = 65536 - text_overhead + 1;
    assert!(body_len < 65536);
    f.accept_fact("budget-growth", &"x".repeat(body_len), &key, Some(1));
    assert_eq!(
        f.error(
            "evidence_sources",
            json!({"inputs":[fact["record_id"]],"max_chars":65536})
        ),
        "resource_exhausted"
    );
}

#[test]
fn fact_binding_is_reread_for_completion_and_audit_even_without_a_revision_change() {
    let mut f = Fixture::new();
    let key = json!({
        "entity":"deployment","attribute":"permission","scope":"tenant A",
        "conditions":"test only"
    });
    let fact = f.accept_fact("key-reread", "Deployment permitted.", &key, None);
    f.create(&[fact["record_id"].clone()]);
    let claim = f.claim();
    let job = f.job(&claim);
    let sql = Connection::open(&f.path).unwrap();
    let tuple =
        |condition: &str| json!(["deployment", "permission", "tenant A", condition]).to_string();
    // Public APIs cannot mutate this identity. Simulate storage tampering to
    // prove that a matching revision alone never attests to a rendered key.
    sql.execute(
        "UPDATE fact_bindings SET fact_key=?1 WHERE namespace='main' AND record_id=?2",
        params![
            tuple("production allowed"),
            fact["record_id"].as_str().unwrap()
        ],
    )
    .unwrap();
    let mut args = lease(&claim);
    args["body"] = job["evidence_body"].clone();
    assert_eq!(f.error("maintenance_complete", args), "fidelity_mismatch");
    sql.execute(
        "UPDATE fact_bindings SET fact_key=?1 WHERE namespace='main' AND record_id=?2",
        params![tuple("test only"), fact["record_id"].as_str().unwrap()],
    )
    .unwrap();
    let done = f.complete(&claim, &job["evidence_body"]);
    assert_eq!(f.audit(&done["id"])["verified"], true);
    sql.execute(
        "UPDATE fact_bindings SET fact_key=?1 WHERE namespace='main' AND record_id=?2",
        params![
            tuple("production allowed"),
            fact["record_id"].as_str().unwrap()
        ],
    )
    .unwrap();
    let audit = f.audit(&done["id"]);
    assert_eq!(audit["verified"], false);
    assert_eq!(audit["fidelity"]["reason"], "content_mismatch");
}

#[test]
fn legacy_job_retains_fact_identity_without_claiming_strict_verification() {
    let mut f = Fixture::new();
    let key = json!({
        "entity":"deployment","attribute":"permission","scope":"tenant A",
        "conditions":"test only"
    });
    let fact = f.accept_fact("legacy-key", "Deployment permitted.", &key, None);
    f.write(
        "summary_create",
        json!({"inputs":[fact["record_id"]],"processor":"synthetic-free-summary"}),
    );
    let claim = f.write(
        "maintenance_claim",
        json!({"worker":"legacy-worker","processor":"synthetic-free-summary"}),
    );
    let job = f.job(&claim);
    assert_eq!(job["inputs"][0]["body"], "Deployment permitted.");
    assert_eq!(job["inputs"][0]["fact_key"], key);
    assert!(job.get("evidence_body").is_none());
    let done = f.complete(
        &claim,
        &json!("Model prose, not verified source preservation."),
    );
    assert_eq!(f.audit(&done["id"])["verified"], false);
}

#[test]
fn full_unicode_assertions_conditions_negations_and_metadata_survive_canonical_rendering() {
    let mut f = Fixture::new();
    let text =
        "不得给 Alice 使用青霉素。\n仅当体温 > 38.5℃，才联系医生；不是自行加药。👩🏽‍💻\t\"原文\" \u{0}";
    let r = f.write(
        "remember",
        json!({"body":text,"kind":"preference","source":"self-reported",
        "volatility":"volatile","valid_from":1,"valid_until":i64::MAX}),
    );
    let before = f.read("status", json!({}));
    let sources = f.read("evidence_sources", json!({"inputs":[r["id"]]}));
    assert_eq!(sources["roots"][0]["body"], text);
    assert_eq!(sources["roots"][0].get("fact_key"), Some(&Value::Null));
    assert_eq!(sources["roots"][0]["valid_until"], i64::MAX);
    assert_eq!(sources["roots"][0]["source"], "self-reported");
    assert_eq!(sources["fidelity"]["semantics_verified"], false);
    assert_eq!(sources["coverage"]["truncated"], false);
    let body: Value = serde_json::from_str(sources["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["source_assertions"][0]["body"], text);
    assert_eq!(
        body["source_assertions"][0].get("fact_key"),
        Some(&Value::Null)
    );
    assert_eq!(body["trust_boundary"], "retrieved_data");
    assert_eq!(
        f.read("status", json!({}))["commit_token"],
        before["commit_token"]
    );
    let recipe = f.create(&[r["id"].clone()]);
    no_body(&recipe, text);
    let done = f.finish();
    no_body(&done, text);
    assert_eq!(done["id"], recipe["output_id"]);
    let audit = f.audit(&done["id"]);
    assert_eq!(audit["verified"], true);
    assert_eq!(audit["body"], sources["body"]);
    assert_eq!(audit["roots"][0]["body"], text);
}

#[test]
fn edges_determine_roots_not_source_names_or_a_derivations_claimed_kind() {
    let mut f = Fixture::new();
    let root = f.remember("The observed value is 7.");
    let invented = f.derive(
        &[root["id"].clone()],
        "The value is definitely 999.",
        "assertion",
    );
    let sources = f.read("evidence_sources", json!({"inputs":[invented["id"]]}));
    assert_eq!(sources["roots"].as_array().unwrap().len(), 1);
    assert_eq!(sources["roots"][0]["id"], root["id"]);
    assert!(!sources["body"].as_str().unwrap().contains("999"));
    assert_eq!(sources["dependencies"].as_array().unwrap().len(), 2);
    for kind in ["summary", "inference"] {
        let orphan = f.write(
            "remember",
            json!({"body":"UNSUPPORTED","kind":kind,"source":PROCESSOR}),
        );
        assert_eq!(
            f.error("evidence_sources", json!({"inputs":[orphan["id"]]})),
            "evidence_sources_missing"
        );
        assert_eq!(
            f.error("evidence_create", json!({"inputs":[orphan["id"]]})),
            "evidence_sources_missing"
        );
        assert_eq!(f.audit(&orphan["id"])["verified"], false);
    }
    assert_eq!(f.audit(&root["id"])["fidelity"]["mode"], "source_assertion");
    assert_eq!(f.audit(&root["id"])["verified"], false);
}

#[test]
fn reserved_processor_cannot_publish_free_text_through_low_level_api() {
    let mut f = Fixture::new();
    let r = f.remember("Negative evidence must survive.");
    f.write(
        "summary_create",
        json!({"inputs":[r["id"]],"processor":PROCESSOR,"max_chars":12000}),
    );
    let claim = f.claim();
    let job = f.job(&claim);
    assert_eq!(job["fidelity"]["mode"], "source_preserving");
    for wrong in [
        json!("Arbitrary fluent summary"),
        json!(format!("{} ", job["evidence_body"].as_str().unwrap())),
        json!(job["evidence_body"]
            .as_str()
            .unwrap()
            .replace("Negative", "Positive")),
    ] {
        let mut args = lease(&claim);
        args["body"] = wrong;
        assert_eq!(f.error("maintenance_complete", args), "fidelity_mismatch");
    }
    let done = f.complete(&claim, &job["evidence_body"]);
    assert_eq!(f.audit(&done["id"])["verified"], true);
    assert_eq!(
        f.error(
            "revise",
            json!({"id":done["id"],"expected_revision":1,"body":"forged"})
        ),
        "managed_record"
    );
}

#[test]
fn recursive_capsules_reread_the_same_roots_and_keep_every_selector_guard() {
    let mut f = Fixture::new();
    let text = "Only for account A, budget ≤ €35.00; never account B.";
    let root = f.remember(text);
    let mut input = root["id"].clone();
    for level in 1..=12 {
        f.create(&[input]);
        let result = f.finish();
        let audit = f.audit(&result["id"]);
        assert_eq!(audit["roots"].as_array().unwrap().len(), 1);
        assert_eq!(audit["roots"][0]["body"], text);
        assert_eq!(audit["dependencies"].as_array().unwrap().len(), level);
        let parsed: Value = serde_json::from_str(audit["body"].as_str().unwrap()).unwrap();
        assert_eq!(parsed["source_assertions"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["source_assertions"][0]["id"], root["id"]);
        input = result["id"].clone();
    }
}

#[test]
fn root_updates_fence_old_worker_and_repair_same_output_without_accumulating_old_claims() {
    let mut f = Fixture::new();
    let root = f.remember("v1: do not enable.");
    let recipe = f.create(&[root["id"].clone()]);
    let stale_claim = f.claim();
    let stale_job = f.job(&stale_claim);
    let mut current = root.clone();
    for version in 2..=18 {
        let text = format!("v{version}: enable only after review; no automatic payment.");
        current = f.write(
            "revise",
            json!({"id":root["id"],"expected_revision":current["revision"],"body":text}),
        );
        if version == 2 {
            let mut args = lease(&stale_claim);
            args["body"] = stale_job["evidence_body"].clone();
            assert_eq!(f.error("maintenance_complete", args), "lease_conflict");
        } else {
            assert_eq!(
                f.error("evidence_inspect", json!({"id":recipe["output_id"]})),
                "stale_dependency"
            );
        }
        let done = f.finish();
        assert_eq!(done["id"], recipe["output_id"]);
        let audit = f.audit(&done["id"]);
        assert_eq!(audit["roots"][0]["body"], text);
        assert_eq!(audit["roots"][0]["revision"], version);
        assert!(!audit["body"].as_str().unwrap().contains("v1: do not"));
    }
}

#[test]
fn intermediate_edit_and_withdrawal_remain_barriers_even_when_root_text_did_not_change() {
    let mut f = Fixture::new();
    let root = f.remember("root original");
    let selector = f.derive(&[root["id"].clone()], "misleading selector text", "summary");
    let recipe = f.create(&[selector["id"].clone()]);
    let old = f.finish();
    assert_eq!(
        f.audit(&old["id"])["dependencies"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let selector = f.write(
        "revise",
        json!({"id":selector["id"],"expected_revision":1,"body":"changed selector"}),
    );
    assert_eq!(
        f.error("evidence_inspect", json!({"id":old["id"]})),
        "stale_dependency"
    );
    let repaired = f.finish();
    assert_eq!(repaired["id"], recipe["output_id"]);
    assert_eq!(repaired["revision"], 2);
    assert_eq!(
        f.audit(&repaired["id"])["roots"][0]["body"],
        "root original"
    );
    f.write(
        "retract",
        json!({"id":selector["id"],"expected_revision":selector["revision"]}),
    );
    assert_eq!(
        f.error("evidence_inspect", json!({"id":repaired["id"]})),
        "not_found"
    );
    assert_eq!(
        f.read("inspect", json!({"id":root["id"]}))["body"],
        "root original"
    );
    assert_eq!(f.claim()["claimed"], false);
}

#[test]
fn stale_legacy_selector_waits_until_repaired_and_does_not_silently_flatten_the_guard() {
    let mut f = Fixture::new();
    let root = f.remember("one");
    let selector = f.derive(&[root["id"].clone()], "legacy selector", "summary");
    f.create(&[selector["id"].clone()]);
    let done = f.finish();
    f.write(
        "revise",
        json!({"id":root["id"],"expected_revision":1,"body":"two"}),
    );
    let poll = f.claim();
    assert_eq!(poll["claimed"], false);
    assert_eq!(poll["deferred_tasks"], 1);
    assert_eq!(poll["state_changed"], true);
    assert_eq!(poll["durable"], true);
    assert_eq!(
        f.error("evidence_inspect", json!({"id":done["id"]})),
        "stale_dependency"
    );
}

#[test]
fn budgets_reject_whole_source_sets_and_growth_fails_safely_then_recovers() {
    let mut f = Fixture::new();
    let source = f.remember("small");
    let before = f.read("status", json!({}));
    assert_eq!(
        f.error(
            "evidence_create",
            json!({"inputs":[source["id"]],"max_chars":10})
        ),
        "resource_exhausted"
    );
    assert_eq!(
        f.read("status", json!({}))["commit_token"],
        before["commit_token"]
    );
    let recipe = f.write(
        "evidence_create",
        json!({"inputs":[source["id"]],"max_chars":2000}),
    );
    f.finish();
    f.write(
        "revise",
        json!({"id":source["id"],"expected_revision":1,"body":"界".repeat(2000)}),
    );
    let poll = f.claim();
    assert_eq!(poll["claimed"], false);
    assert_eq!(poll["failed_tasks"], 1);
    assert_eq!(poll["durable"], true);
    let status = f.read("maintenance_status", json!({}));
    assert_eq!(status["tasks"][0]["state"], "failed");
    assert_eq!(status["tasks"][0]["error_code"], "resource_exhausted");
    assert_eq!(
        f.error("evidence_inspect", json!({"id":recipe["output_id"]})),
        "stale_dependency"
    );
    f.write(
        "revise",
        json!({"id":source["id"],"expected_revision":2,"body":"small again"}),
    );
    let done = f.finish();
    assert_eq!(done["id"], recipe["output_id"]);
    assert_eq!(f.audit(&done["id"])["roots"][0]["body"], "small again");
    let huge = f.remember(&"🧪".repeat(16_300));
    assert_eq!(
        f.error(
            "evidence_sources",
            json!({"inputs":[huge["id"]],"max_chars":65536})
        ),
        "resource_exhausted"
    );
}

#[test]
fn graph_node_depth_and_cycle_limits_fail_closed() {
    let mut f = Fixture::new();
    let mut roots = Vec::new();
    for _ in 0..64 {
        roots.push(f.remember("r")["id"].clone());
    }
    let wide = f.derive(&roots, "wide", "summary");
    assert_eq!(
        f.error(
            "evidence_sources",
            json!({"inputs":[wide["id"]],"max_chars":65536})
        ),
        "resource_exhausted"
    );
    let mut chain = roots[0].clone();
    for _ in 0..33 {
        chain = f.derive(&[chain], "deep", "summary")["id"].clone();
    }
    assert_eq!(
        f.error("evidence_sources", json!({"inputs":[chain]})),
        "resource_exhausted"
    );
    let sql = Connection::open(&f.path).unwrap();
    sql.execute(
        "INSERT INTO edges VALUES(?1,?1,1)",
        [roots[1].as_str().unwrap()],
    )
    .unwrap();
    assert_eq!(
        f.error("evidence_sources", json!({"inputs":[roots[1]]})),
        "invalid_dependency"
    );
}

#[test]
fn duplicate_diamond_paths_are_deduplicated_without_dropping_guards() {
    let mut f = Fixture::new();
    let root = f.remember("shared assertion");
    let left = f.derive(&[root["id"].clone()], "left", "summary");
    let right = f.derive(&[root["id"].clone()], "right", "summary");
    let top = f.derive(&[left["id"].clone(), right["id"].clone()], "top", "summary");
    let source = f.read("evidence_sources", json!({"inputs":[top["id"],root["id"]]}));
    assert_eq!(source["roots"].as_array().unwrap().len(), 1);
    assert_eq!(source["dependencies"].as_array().unwrap().len(), 4);
    assert_eq!(
        source["coverage"]["root_utf8_bytes"],
        "shared assertion".len()
    );
}

#[test]
fn namespace_validity_and_input_contract_are_enforced_before_publication() {
    let mut f = Fixture::new();
    let other = f
        .store
        .execute(
            "other",
            "remember",
            json!({"body":"private","idempotency_key":"other"}),
        )
        .unwrap();
    assert_eq!(
        f.error("evidence_sources", json!({"inputs":[other["id"]]})),
        "not_found"
    );
    let future = f.write("remember", json!({"body":"future","valid_from":i64::MAX-1}));
    let expired = f.write("remember", json!({"body":"expired","valid_until":1}));
    for id in [&future["id"], &expired["id"]] {
        assert_eq!(
            f.error("evidence_create", json!({"inputs":[id]})),
            "stale_dependency"
        );
    }
    let r = f.remember("source");
    for args in [
        json!({"inputs":[]}),
        json!({"inputs":[r["id"],r["id"]]}),
        json!({"inputs":[r["id"]],"processor":"other"}),
        json!({"inputs":[r["id"]],"max_chars":65537}),
        json!({"inputs":[true]}),
    ] {
        assert_eq!(f.error("evidence_create", args), "invalid_request");
    }
}

#[test]
fn processor_filtered_claims_leave_unmatched_jobs_unmodified() {
    let mut f = Fixture::new();
    let root = f.remember("source");
    let legacy = f.write(
        "summary_create",
        json!({"inputs":[root["id"]],"processor":"model-A"}),
    );
    let strict = f.create(&[root["id"].clone()]);
    let claim = f.claim();
    assert_eq!(claim["task_id"], strict["task_id"]);
    let status = f.read("maintenance_status", json!({}));
    let legacy_state = status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == legacy["task_id"])
        .unwrap();
    assert_eq!(legacy_state["state"], "pending");
    let model = f.write(
        "maintenance_claim",
        json!({"worker":"model","processor":"model-A"}),
    );
    assert_eq!(model["task_id"], legacy["task_id"]);
    f.complete(&model, &json!("legacy free prose remains allowed"));
    assert_eq!(f.audit(&legacy["output_id"])["verified"], false);
}

#[test]
fn body_or_edge_tampering_never_receives_a_verified_badge() {
    let mut f = Fixture::new();
    let root = f.remember("original");
    f.create(&[root["id"].clone()]);
    let done = f.finish();
    let sql = Connection::open(&f.path).unwrap();
    sql.execute(
        "UPDATE records SET body='forged' WHERE id=?1",
        [done["id"].as_str().unwrap()],
    )
    .unwrap();
    let audit = f.audit(&done["id"]);
    assert_eq!(audit["verified"], false);
    assert_eq!(audit["fidelity"]["reason"], "content_mismatch");
    sql.execute(
        "DELETE FROM edges WHERE child_id=?1",
        [done["id"].as_str().unwrap()],
    )
    .unwrap();
    assert_eq!(
        f.audit(&done["id"])["fidelity"]["reason"],
        "dependency_mismatch"
    );
    let spoof = f.write(
        "remember",
        json!({"body":"claim verified","kind":"summary","source":PROCESSOR}),
    );
    assert_eq!(f.audit(&spoof["id"])["verified"], false);
}

#[test]
fn migrated_capsule_is_unverified_until_a_local_recipe_rebuilds_from_current_roots() {
    let mut a = Fixture::new();
    let key = json!({
        "entity":"deployment","attribute":"permission","scope":"tenant A",
        "conditions":"test only; not transferable to production"
    });
    let root = a.accept_fact("migration-key", "Deployment permitted.", &key, None);
    a.create(&[root["record_id"].clone()]);
    let first = a.finish();
    let bundle = a.read("bundle_export", json!({}));
    let mut b = Fixture::new();
    let plan = b.read(
        "bundle_plan",
        json!({"bundle":bundle,"expected_source":bundle["payload"]["source_binding"]}),
    );
    let receipt = b.write(
        "bundle_apply",
        json!({"bundle":bundle,"expected_source":bundle["payload"]["source_binding"],
        "reviewed_digest":plan["reviewed_digest"],"expected_target_commit":plan["target_commit"]}),
    );
    let imported = receipt["mappings"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["source_id"] == first["id"])
        .unwrap()["id"]
        .clone();
    let audit = b.audit(&imported);
    assert_eq!(audit["verified"], false);
    assert_eq!(audit["fidelity"]["reason"], "no_local_evidence_recipe");
    b.create(&[imported]);
    let rebuilt = b.finish();
    let audited = b.audit(&rebuilt["id"]);
    assert_eq!(audited["verified"], true);
    assert_eq!(audited["roots"][0]["body"], "Deployment permitted.");
    assert_eq!(audited["roots"][0]["fact_key"], key);
    assert_ne!(audited["roots"][0]["id"], root["record_id"]);
    let body: Value = serde_json::from_str(audited["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["source_assertions"][0]["fact_key"], key);
}

#[test]
fn queued_graph_growth_failures_are_bounded_and_do_not_block_later_work() {
    let mut f = Fixture::new();
    let grow = f.remember("small");
    for _ in 0..65 {
        f.write(
            "evidence_create",
            json!({"inputs":[grow["id"]],"max_chars":1000}),
        );
    }
    f.write(
        "revise",
        json!({"id":grow["id"],"expected_revision":1,"body":"big ".repeat(1000)}),
    );
    let small = f.remember("still eligible");
    let eligible = f.create(&[small["id"].clone()]);
    let first = f.claim();
    assert_eq!(first["claimed"], false);
    assert_eq!(first["failed_tasks"], 64);
    assert_eq!(first["state_changed"], true);
    let second = f.claim();
    assert_eq!(second["claimed"], true);
    assert_eq!(second["failed_tasks"], 1);
    assert_eq!(second["task_id"], eligible["task_id"]);
}

#[test]
fn durable_receipts_have_no_root_body_and_cannot_reauthorize_withdrawn_content() {
    let mut f = Fixture::new();
    let text = "sensitive assertion";
    let root = f.remember(text);
    let args = json!({"inputs":[root["id"]],"idempotency_key":"create-once"});
    let recipe = f.read("evidence_create", args.clone());
    f.store = Store::open(&f.path).unwrap();
    assert_eq!(f.read("evidence_create", args), recipe);
    let done = f.finish();
    f.write("retract", json!({"id":root["id"],"expected_revision":1}));
    assert_eq!(
        f.error("evidence_inspect", json!({"id":done["id"]})),
        "not_found"
    );
    let sql = Connection::open(&f.path).unwrap();
    let receipts:Vec<String>=sql.prepare("SELECT receipt FROM operations WHERE method IN ('evidence_create','maintenance_claim','maintenance_complete')")
        .unwrap().query_map([],|r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
    assert!(receipts.iter().all(|r| !r.contains(text)));
    let retained: i64 = sql
        .query_row(
            "SELECT count(*) FROM revisions WHERE id=?1 AND body!=''",
            params![done["id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retained, 0);
}
