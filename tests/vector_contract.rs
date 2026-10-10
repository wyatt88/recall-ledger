//! Independent black-box vector and migration checks using synthetic data only.
//! Fixed vectors exercise retrieval mechanics, not an embedding model's quality.
use recall_ledger::Store;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Barrier};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const NS: &str = "vector-contract";
const MODEL: &str = "synthetic-space/revision-1";

fn database() -> (TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    (directory, store)
}

fn call(store: &mut Store, method: &str, args: Value) -> Value {
    store
        .execute(NS, method, args)
        .unwrap_or_else(|error| panic!("{method} failed: {error}"))
}

fn remember(store: &mut Store, key: &str, text: &str) -> Value {
    call(
        store,
        "remember",
        json!({"body":text,"idempotency_key":key}),
    )
}

fn id(value: &Value) -> &str {
    value["id"].as_str().unwrap()
}

fn embed(store: &mut Store, record: &Value, key: &str, model: &str, vector: Value) -> Value {
    call(
        store,
        "embedding_set",
        json!({
            "id":id(record),"expected_revision":record["revision"],
            "model_key":model,"vector":vector,"idempotency_key":key
        }),
    )
}

fn search(store: &mut Store, model: &str, vector: Value) -> Value {
    call(
        store,
        "recall",
        json!({
            "mode":"vector","model_key":model,"query_vector":vector,
            "limit":100,"max_chars":100_000
        }),
    )
}

fn ids(value: &Value) -> Vec<String> {
    value["items"]
        .as_array()
        .expect("recall response must have items")
        .iter()
        .map(|record| id(record).to_owned())
        .collect()
}

fn derive(store: &mut Store, key: &str, inputs: &[&str], text: &str) -> Value {
    let prepared = call(
        store,
        "prepare",
        json!({
            "inputs":inputs,"processor":"synthetic-transform",
            "idempotency_key":format!("{key}-prepare")
        }),
    );
    call(
        store,
        "publish",
        json!({
            "job_id":prepared["job_id"],"body":text,
            "idempotency_key":format!("{key}-publish")
        }),
    )
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn cosine_orders_by_direction_and_does_not_require_lexical_overlap() {
    let (_directory, mut store) = database();
    let nearest = remember(&mut store, "nearest", "a feline resting quietly");
    let orthogonal = remember(&mut store, "orthogonal", "database write journal");
    let opposite = remember(&mut store, "opposite", "cobalt aquatic mineral");
    embed(&mut store, &nearest, "v-near", MODEL, json!([10.0, 0.0]));
    embed(
        &mut store,
        &orthogonal,
        "v-orthogonal",
        MODEL,
        json!([0.0, 10_000.0]),
    );
    embed(
        &mut store,
        &opposite,
        "v-opposite",
        MODEL,
        json!([-1.0, 0.0]),
    );
    let result = call(
        &mut store,
        "recall",
        json!({
            "query":"the query has no shared terms","mode":"vector",
            "model_key":MODEL,"query_vector":[1.0,0.0],
            "limit":100,"max_chars":100_000
        }),
    );
    assert_eq!(
        ids(&result),
        vec![id(&nearest), id(&orthogonal), id(&opposite)]
    );
}

#[test]
fn hybrid_fuses_independent_rankings_instead_of_filtering_vector_hits() {
    let (_directory, mut store) = database();
    let both = remember(&mut store, "both", "orchard boundary maintenance");
    let vector_only = remember(&mut store, "vector-only", "gentle tropical rainfall");
    embed(&mut store, &both, "v-both", MODEL, json!([0.8, 0.6]));
    embed(
        &mut store,
        &vector_only,
        "v-vector-only",
        MODEL,
        json!([1.0, 0.0]),
    );
    let pure_vector = search(&mut store, MODEL, json!([1.0, 0.0]));
    assert_eq!(ids(&pure_vector)[0], id(&vector_only));
    let hybrid = call(
        &mut store,
        "recall",
        json!({
            "query":"orchard","mode":"hybrid","model_key":MODEL,
            "query_vector":[1.0,0.0],"limit":100,"max_chars":100_000
        }),
    );
    assert_eq!(ids(&hybrid), vec![id(&both), id(&vector_only)]);
    let lexical = call(
        &mut store,
        "recall",
        json!({"query":"orchard","mode":"lexical"}),
    );
    assert_eq!(ids(&lexical), vec![id(&both)]);
}

#[test]
fn model_identity_and_namespace_isolate_equal_dimension_vectors() {
    let (_directory, mut store) = database();
    let first = remember(&mut store, "first", "space-one-marker");
    let second = remember(&mut store, "second", "space-two-marker");
    embed(&mut store, &first, "v-first", MODEL, json!([1.0, 0.0]));
    embed(
        &mut store,
        &second,
        "v-second",
        "other-space/revision-1",
        json!([1.0, 0.0]),
    );
    let foreign = store
        .execute(
            "foreign",
            "remember",
            json!({"body":"foreign-secret-marker","idempotency_key":"foreign"}),
        )
        .unwrap();
    store
        .execute(
            "foreign",
            "embedding_set",
            json!({
                "id":id(&foreign),"expected_revision":1,"model_key":MODEL,
                "vector":[1.0,0.0],"idempotency_key":"foreign-vector"
            }),
        )
        .unwrap();
    assert_eq!(
        ids(&search(&mut store, MODEL, json!([1.0, 0.0]))),
        vec![id(&first)]
    );
    assert_eq!(
        ids(&search(
            &mut store,
            "other-space/revision-1",
            json!([1.0, 0.0])
        )),
        vec![id(&second)]
    );
    assert!(store
        .execute(
            "foreign",
            "embedding_set",
            json!({
                "id":id(&first),"expected_revision":1,"model_key":MODEL,
                "vector":[1.0,0.0],"idempotency_key":"cross-namespace"
            }),
        )
        .is_err());
}

#[test]
fn a_model_key_cannot_mix_dimensions_or_accept_wrong_dimension_queries() {
    let (_directory, mut store) = database();
    let first = remember(&mut store, "first", "first dimension source");
    let second = remember(&mut store, "second", "second dimension source");
    embed(&mut store, &first, "v-first", MODEL, json!([1.0, 0.0]));
    assert!(store
        .execute(
            NS,
            "embedding_set",
            json!({
                "id":id(&second),"expected_revision":1,"model_key":MODEL,
                "vector":[1.0,0.0,0.0],"idempotency_key":"mixed-dimensions"
            }),
        )
        .is_err());
    assert!(store
        .execute(
            NS,
            "recall",
            json!({"mode":"vector","model_key":MODEL,"query_vector":[1.0,0.0,0.0]}),
        )
        .is_err());
    assert_eq!(
        ids(&search(&mut store, MODEL, json!([1.0, 0.0]))),
        vec![id(&first)]
    );
    call(
        &mut store,
        "retract",
        json!({"id":id(&first),"expected_revision":1,"idempotency_key":"withdraw-first"}),
    );
    assert!(store
        .execute(
            NS,
            "embedding_set",
            json!({
                "id":id(&second),"expected_revision":1,"model_key":MODEL,
                "vector":[1.0,0.0,0.0],"idempotency_key":"rebind-empty-model"
            }),
        )
        .is_err());
}

#[test]
fn indexing_candidates_are_bounded_current_and_namespace_scoped() {
    let (_directory, mut store) = database();
    let root = remember(&mut store, "root", "live indexing source");
    let stale = derive(&mut store, "stale", &[id(&root)], "stale index body");
    call(
        &mut store,
        "revise",
        json!({
            "id":id(&root),"expected_revision":1,"body":"current indexing source",
            "idempotency_key":"revise-root"
        }),
    );
    let ready = remember(&mut store, "ready", "already indexed");
    embed(&mut store, &ready, "ready-vector", MODEL, json!([1.0, 0.0]));
    let pending = remember(&mut store, "pending", "awaiting index");
    for (key, bounds) in [
        ("expired", json!({"valid_until":timestamp()-1})),
        ("future", json!({"valid_from":timestamp()+3600})),
    ] {
        let mut args = bounds;
        args["body"] = json!(format!("{key} index body"));
        args["idempotency_key"] = json!(key);
        call(&mut store, "remember", args);
    }
    store
        .execute(
            "foreign",
            "remember",
            json!({"body":"foreign index body","idempotency_key":"foreign"}),
        )
        .unwrap();
    let first = call(
        &mut store,
        "embedding_candidates",
        json!({"model_key":MODEL,"limit":1}),
    );
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    let all = call(
        &mut store,
        "embedding_candidates",
        json!({"model_key":MODEL,"limit":32}),
    );
    let mut actual = ids(&all);
    actual.sort();
    let mut expected = vec![id(&root).to_owned(), id(&pending).to_owned()];
    expected.sort();
    assert_eq!(actual, expected);
    assert!(!actual.contains(&id(&stale).to_owned()));
    assert!(!all.to_string().contains("stale index body"));
    assert!(!all.to_string().contains("expired index body"));
    assert!(!all.to_string().contains("future index body"));
    assert!(!all.to_string().contains("foreign index body"));
    for candidate in all["items"].as_array().unwrap() {
        embed(
            &mut store,
            candidate,
            &format!("index-{}", id(candidate)),
            MODEL,
            json!([1.0, 0.0]),
        );
    }
    let finished = call(
        &mut store,
        "embedding_candidates",
        json!({"model_key":MODEL}),
    );
    assert!(ids(&finished).is_empty());
    assert_eq!(finished["indexed"], 3);
}

#[test]
fn invalid_vectors_do_not_commit_and_valid_extreme_floats_stay_ordered() {
    let (_directory, mut store) = database();
    let source = remember(&mut store, "source", "numeric source");
    for (index, vector) in [
        json!([]),
        json!([0.0, 0.0]),
        json!([null, 1.0]),
        json!([1.0, "2"]),
        json!("not-a-vector"),
        json!([true, false]),
    ]
    .into_iter()
    .enumerate()
    {
        let key = format!("invalid-{index}");
        assert!(
            store
                .execute(
                    NS,
                    "embedding_set",
                    json!({
                        "id":id(&source),"expected_revision":1,"model_key":MODEL,
                        "vector":vector,"idempotency_key":key
                    }),
                )
                .is_err(),
            "invalid vector accepted"
        );
        assert!(store
            .execute(
                NS,
                "operation",
                json!({"method":"embedding_set","idempotency_key":key}),
            )
            .is_err());
    }
    let other = remember(&mut store, "other", "extreme opposite");
    embed(&mut store, &source, "huge", MODEL, json!([1.0e30, 1.0e30]));
    embed(
        &mut store,
        &other,
        "tiny",
        MODEL,
        json!([-1.0e-30, -1.0e-30]),
    );
    let result = search(&mut store, MODEL, json!([1.0e30, 1.0e30]));
    assert_eq!(ids(&result), vec![id(&source), id(&other)]);
    for item in result["items"].as_array().unwrap() {
        assert!(item["selection_score"].as_f64().unwrap().is_finite());
    }
}

#[test]
fn source_revision_removes_old_vector_and_rejects_late_embedding() {
    let (_directory, mut store) = database();
    let source = remember(&mut store, "source", "before revision");
    embed(
        &mut store,
        &source,
        "first-vector",
        MODEL,
        json!([1.0, 0.0]),
    );
    let current = call(
        &mut store,
        "revise",
        json!({
            "id":id(&source),"expected_revision":1,"body":"after revision",
            "idempotency_key":"revise"
        }),
    );
    assert!(ids(&search(&mut store, MODEL, json!([1.0, 0.0]))).is_empty());
    assert!(store
        .execute(
            NS,
            "embedding_set",
            json!({
                "id":id(&source),"expected_revision":1,"model_key":MODEL,
                "vector":[1.0,0.0],"idempotency_key":"late-vector"
            }),
        )
        .is_err());
    embed(
        &mut store,
        &current,
        "current-vector",
        MODEL,
        json!([0.0, 1.0]),
    );
    let result = search(&mut store, MODEL, json!([0.0, 1.0]));
    assert_eq!(ids(&result), vec![id(&source)]);
    assert!(!result.to_string().contains("before revision"));
    assert_eq!(result["items"][0]["revision"], 2);
}

#[test]
fn transitive_staleness_and_retraction_also_close_vector_retrieval() {
    let (_directory, mut store) = database();
    let root = remember(&mut store, "root", "sensitive root");
    let first = derive(&mut store, "first", &[id(&root)], "sensitive first summary");
    let second = derive(
        &mut store,
        "second",
        &[id(&first)],
        "sensitive second summary",
    );
    let third = derive(
        &mut store,
        "third",
        &[id(&second)],
        "sensitive third summary",
    );
    let independent = remember(&mut store, "independent", "unrelated survivor");
    for (index, record) in [&root, &first, &second, &third, &independent]
        .into_iter()
        .enumerate()
    {
        embed(
            &mut store,
            record,
            &format!("index-{index}"),
            MODEL,
            json!([1.0, 0.0]),
        );
    }
    call(
        &mut store,
        "revise",
        json!({
            "id":id(&root),"expected_revision":1,"body":"replacement root",
            "idempotency_key":"revision"
        }),
    );
    assert_eq!(
        ids(&search(&mut store, MODEL, json!([1.0, 0.0]))),
        vec![id(&independent)]
    );
    assert!(store
        .execute(
            NS,
            "embedding_set",
            json!({
                "id":id(&third),"expected_revision":1,"model_key":MODEL,
                "vector":[1.0,0.0],"idempotency_key":"index-stale"
            }),
        )
        .is_err());
    call(
        &mut store,
        "retract",
        json!({"id":id(&root),"expected_revision":2,"idempotency_key":"withdraw"}),
    );
    let recovered = call(
        &mut store,
        "embedding_set",
        json!({
            "id":id(&third),"expected_revision":1,"model_key":MODEL,
            "vector":[1.0,0.0],"idempotency_key":"index-3"
        }),
    );
    assert!(!recovered.to_string().contains("sensitive"));
    let result = search(&mut store, MODEL, json!([1.0, 0.0]));
    assert_eq!(ids(&result), vec![id(&independent)]);
    assert!(!result.to_string().contains("sensitive"));
}

#[test]
fn vector_reads_recheck_ancestor_expiry_without_needing_a_mutation() {
    let (_directory, mut store) = database();
    let expires = timestamp() + 2;
    let root = call(
        &mut store,
        "remember",
        json!({
            "body":"expiring input","valid_until":expires,
            "idempotency_key":"expiring-root"
        }),
    );
    let summary = derive(&mut store, "summary", &[id(&root)], "expiring derived text");
    embed(&mut store, &summary, "vector", MODEL, json!([1.0, 0.0]));
    assert_eq!(
        ids(&search(&mut store, MODEL, json!([1.0, 0.0]))),
        vec![id(&summary)]
    );
    while timestamp() <= expires {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(ids(&search(&mut store, MODEL, json!([1.0, 0.0]))).is_empty());
    assert!(store
        .execute(
            NS,
            "embedding_set",
            json!({
                "id":id(&summary),"expected_revision":1,"model_key":MODEL,
                "vector":[1.0,0.0],"idempotency_key":"late-expired"
            }),
        )
        .is_err());
}

#[test]
fn vector_receipts_are_idempotent_and_vectors_survive_a_restart() {
    let (directory, mut store) = database();
    let source = remember(&mut store, "source", "restart survivor");
    let args = json!({
        "id":id(&source),"expected_revision":1,"model_key":MODEL,
        "vector":[1.0,0.0],"idempotency_key":"vector-once"
    });
    let original = call(&mut store, "embedding_set", args.clone());
    assert_eq!(call(&mut store, "embedding_set", args.clone()), original);
    assert!(!original.to_string().contains("restart survivor"));
    assert!(original.get("vector").is_none());
    drop(store);
    let mut reopened = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    assert_eq!(call(&mut reopened, "embedding_set", args), original);
    assert_eq!(
        ids(&search(&mut reopened, MODEL, json!([1.0, 0.0]))),
        vec![id(&source)]
    );
    assert!(reopened
        .execute(
            NS,
            "embedding_set",
            json!({
                "id":id(&source),"expected_revision":1,"model_key":MODEL,
                "vector":[0.0,1.0],"idempotency_key":"vector-once"
            }),
        )
        .is_err());
}

// This is the released v0.1 schema, with synthetic rows. Its creation deliberately
// does not use Store::open so migration cannot accidentally test only fresh v0.2 DBs.
fn v1_fixture(path: &std::path::Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
         CREATE TABLE records(
           id TEXT PRIMARY KEY,namespace TEXT NOT NULL,revision INTEGER NOT NULL,
           body TEXT NOT NULL,kind TEXT NOT NULL,source TEXT NOT NULL,
           volatility TEXT NOT NULL,status TEXT NOT NULL,
           valid_from INTEGER,valid_until INTEGER,created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL);
         CREATE INDEX records_scope ON records(namespace,status);
         CREATE TABLE revisions(
           id TEXT NOT NULL,revision INTEGER NOT NULL,namespace TEXT NOT NULL,
           body TEXT NOT NULL,recorded_at INTEGER NOT NULL,
           PRIMARY KEY(id,revision),FOREIGN KEY(id) REFERENCES records(id));
         CREATE TABLE edges(
           child_id TEXT NOT NULL,parent_id TEXT NOT NULL,parent_revision INTEGER NOT NULL,
           PRIMARY KEY(child_id,parent_id),FOREIGN KEY(child_id) REFERENCES records(id),
           FOREIGN KEY(parent_id) REFERENCES records(id));
         CREATE INDEX edges_parent ON edges(parent_id);
         CREATE TABLE jobs(
           id TEXT PRIMARY KEY,namespace TEXT NOT NULL,processor TEXT NOT NULL,
           state TEXT NOT NULL,output_id TEXT,created_at INTEGER NOT NULL);
         CREATE TABLE job_inputs(
           job_id TEXT NOT NULL,record_id TEXT NOT NULL,revision INTEGER NOT NULL,
           PRIMARY KEY(job_id,record_id),FOREIGN KEY(job_id) REFERENCES jobs(id),
           FOREIGN KEY(record_id) REFERENCES records(id));
         CREATE TABLE operations(
           namespace TEXT NOT NULL,method TEXT NOT NULL,key TEXT NOT NULL,
           request_hash TEXT NOT NULL,receipt TEXT NOT NULL,
           PRIMARY KEY(namespace,method,key));
         CREATE VIRTUAL TABLE record_fts USING fts5(id UNINDEXED,namespace UNINDEXED,body);
         INSERT INTO meta VALUES('store_id','legacy-history'),('sequence','7');
         PRAGMA user_version=1;",
    )
    .unwrap();
    for (id, body, kind) in [
        ("legacy-root", "legacy root fact", "assertion"),
        ("legacy-summary", "legacy derived fact", "summary"),
    ] {
        db.execute(
            "INSERT INTO records VALUES(?1,?2,1,?3,?4,'fixture','normal','active',NULL,NULL,1,1)",
            params![id, NS, body, kind],
        )
        .unwrap();
        db.execute(
            "INSERT INTO revisions VALUES(?1,1,?2,?3,1)",
            params![id, NS, body],
        )
        .unwrap();
        db.execute(
            "INSERT INTO record_fts VALUES(?1,?2,?3)",
            params![id, NS, body],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO edges VALUES('legacy-summary','legacy-root',1)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO jobs VALUES('legacy-job',?1,'fixture','prepared',NULL,1)",
        [NS],
    )
    .unwrap();
    db.execute(
        "INSERT INTO job_inputs VALUES('legacy-job','legacy-root',1)",
        [],
    )
    .unwrap();
    let original = json!({"body":"legacy root fact","idempotency_key":"legacy-remember"});
    let hash = format!("{:x}", Sha256::digest(original.to_string().as_bytes()));
    let receipt = json!({
        "id":"legacy-root","revision":1,"state":"active","index_state":"lexical_ready",
        "commit_token":"legacy-history:7","durable":true
    });
    db.execute(
        "INSERT INTO operations VALUES(?1,'remember','legacy-remember',?2,?3)",
        params![NS, hash, receipt.to_string()],
    )
    .unwrap();
}

#[test]
fn concurrent_v1_migration_preserves_history_receipts_and_prepared_work() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.sqlite3");
    v1_fixture(&path);
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut store = Store::open(path).unwrap();
                store.execute(NS, "status", json!({})).unwrap()
            })
        })
        .collect();
    for thread in threads {
        let status = thread.join().unwrap();
        assert_eq!(status["schema_version"], 2);
        assert_eq!(status["commit_token"], "legacy-history:7");
    }
    let mut store = Store::open(&path).unwrap();
    let original = call(
        &mut store,
        "remember",
        json!({"body":"legacy root fact","idempotency_key":"legacy-remember"}),
    );
    assert_eq!(id(&original), "legacy-root");
    assert_eq!(original["commit_token"], "legacy-history:7");
    let history = call(
        &mut store,
        "inspect",
        json!({"id":"legacy-root","history":true}),
    );
    assert_eq!(history["history"][0]["body"], "legacy root fact");
    let visible = call(
        &mut store,
        "recall",
        json!({"query":"legacy","minimum_commit":"legacy-history:7"}),
    );
    assert_eq!(ids(&visible).len(), 2);
    let published = call(
        &mut store,
        "publish",
        json!({
            "job_id":"legacy-job","body":"completed old work",
            "idempotency_key":"legacy-publish"
        }),
    );
    assert_eq!(published["commit_token"], "legacy-history:8");
    embed(
        &mut store,
        &published,
        "migrated-vector",
        MODEL,
        json!([1.0, 0.0]),
    );
    assert_eq!(
        ids(&search(&mut store, MODEL, json!([1.0, 0.0]))),
        vec![id(&published)]
    );
    call(
        &mut store,
        "revise",
        json!({
            "id":"legacy-root","expected_revision":1,"body":"current fact",
            "idempotency_key":"revise-legacy"
        }),
    );
    assert!(store
        .execute(NS, "inspect", json!({"id":"legacy-summary"}))
        .is_err());
    assert!(ids(&search(&mut store, MODEL, json!([1.0, 0.0]))).is_empty());
}

#[test]
fn unknown_future_schema_is_rejected_without_initialization() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("future.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA user_version=999;").unwrap();
    drop(db);
    assert!(Store::open(&path).is_err());
    let db = Connection::open(&path).unwrap();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 999);
    let tables: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}
