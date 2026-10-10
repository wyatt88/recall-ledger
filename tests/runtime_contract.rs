//! Independent runtime checks with synthetic loopback HTTP peers, never real models.
use recall_ledger::models::{Config, EmbeddingConfig, SummaryConfig};
use recall_ledger::runtime::Runtime;
use recall_ledger::Store;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tempfile::TempDir;

const NS: &str = "runtime-contract";

struct Peer {
    url: String,
    stop: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl Peer {
    fn new(handler: impl Fn(Value) -> Value + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1/test", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let thread_count = count.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("synthetic model listener failed: {error}"),
                };
                // macOS inherits the listening socket's nonblocking flag.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut headers = Vec::new();
                loop {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                    assert!(headers.len() < 16_384);
                    if headers.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8(headers).unwrap();
                let length = text
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .unwrap()
                    .1
                    .trim()
                    .parse::<usize>()
                    .unwrap();
                assert!(length <= 262_144);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                thread_count.fetch_add(1, Ordering::SeqCst);
                let response = handler(serde_json::from_slice(&body).unwrap());
                let bytes = serde_json::to_vec(&response).unwrap();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        Self {
            url,
            stop,
            count,
            thread: Some(thread),
        }
    }

    fn calls(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn config(&self, embedding: bool, summary: bool) -> Config {
        Config {
            embedding: embedding.then(|| EmbeddingConfig {
                url: self.url.clone(),
                model: "fixture-embedding".into(),
                revision: "fixture-weights-v1".into(),
                dimensions: Some(2),
                task_field: None,
                timeout_seconds: 5,
            }),
            summary: summary.then(|| SummaryConfig {
                url: self.url.clone(),
                model: "fixture-summary".into(),
                revision: "fixture-weights-v1".into(),
                timeout_seconds: 5,
                max_tokens: 128,
                extra_body: None,
            }),
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !thread::panicking() {
                result.expect("synthetic model thread failed");
            }
        }
    }
}

fn reply(request: Value) -> Value {
    if let Some(input) = request["input"].as_str() {
        let vector = if input.contains("cat") || input.contains("feline") {
            json!([1.0, 0.0])
        } else {
            json!([0.0, 1.0])
        };
        json!({"model":"fixture-embedding","data":[{"index":0,"embedding":vector}]})
    } else {
        let envelope: Value =
            serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
        let text = envelope["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["text"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join(" | ");
        json!({
            "model":"fixture-summary",
            "choices":[{"finish_reason":"stop","message":{"role":"assistant","content":text}}]
        })
    }
}

fn database(config: Option<Config>) -> (TempDir, Runtime) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    let runtime = Runtime::new(store, config).unwrap();
    (directory, runtime)
}

fn call(runtime: &mut Runtime, method: &str, args: Value) -> Value {
    runtime
        .execute(NS, method, args)
        .unwrap_or_else(|error| panic!("{method} failed: {error}"))
}

fn remember(runtime: &mut Runtime, key: &str, body: &str) -> Value {
    call(
        runtime,
        "remember",
        json!({"body":body,"idempotency_key":key}),
    )
}

fn tick(runtime: &mut Runtime) -> Value {
    call(
        runtime,
        "maintain",
        json!({"max_embeddings":4,"max_summaries":1,"auto_summary_min":0}),
    )
}

#[test]
fn configured_runtime_indexes_records_and_embeds_query_without_supplied_vectors() {
    let peer = Peer::new(reply);
    let (_directory, mut runtime) = database(Some(peer.config(true, false)));
    let first = remember(&mut runtime, "first", "A cat sleeps on the sofa.");
    remember(&mut runtime, "second", "SQLite stores a write journal.");
    tick(&mut runtime);
    assert_eq!(peer.calls(), 2, "one bounded indexing call per source");
    let result = call(
        &mut runtime,
        "recall",
        json!({"query":"feline companion","mode":"vector"}),
    );
    assert_eq!(result["items"][0]["id"], first["id"]);
    assert_eq!(peer.calls(), 3, "query embedding is a real HTTP request");
    tick(&mut runtime);
    assert_eq!(
        peer.calls(),
        3,
        "up-to-date vectors must not be regenerated"
    );
}

#[test]
fn embedding_dimension_drift_enters_backoff_instead_of_repeating_each_tick() {
    let changed = Arc::new(AtomicBool::new(false));
    let server_changed = changed.clone();
    let peer = Peer::new(move |_| {
        let vector = if server_changed.load(Ordering::SeqCst) {
            json!([1.0, 0.0, 0.0])
        } else {
            json!([1.0, 0.0])
        };
        json!({"model":"fixture-embedding","data":[{"index":0,"embedding":vector}]})
    });
    let mut config = peer.config(true, false);
    config.embedding.as_mut().unwrap().dimensions = None;
    let (_directory, mut runtime) = database(Some(config));
    remember(
        &mut runtime,
        "initial",
        "First vector establishes the dimension.",
    );
    tick(&mut runtime);
    assert_eq!(peer.calls(), 1);
    changed.store(true, Ordering::SeqCst);
    remember(
        &mut runtime,
        "new",
        "The same declared model now returns a changed shape.",
    );
    tick(&mut runtime);
    assert_eq!(peer.calls(), 2);
    let status = call(&mut runtime, "embedding_status", json!({}));
    assert_eq!(status["retry_wait"], 1);
    tick(&mut runtime);
    assert_eq!(
        peer.calls(),
        2,
        "failed vector commits must respect backoff too"
    );
}

#[test]
fn configured_summary_repairs_same_output_and_retraction_stops_model_work() {
    let peer = Peer::new(reply);
    let (_directory, mut runtime) = database(Some(peer.config(true, true)));
    let source = remember(
        &mut runtime,
        "source",
        "The implementation language is Rust.",
    );
    let recipe = call(
        &mut runtime,
        "summarize",
        json!({"inputs":[source["id"]],"max_chars":200,"idempotency_key":"summary"}),
    );
    tick(&mut runtime);
    let first = call(&mut runtime, "inspect", json!({"id":recipe["output_id"]}));
    assert_eq!(first["revision"], 1);
    assert!(first["body"].as_str().unwrap().contains("Rust"));
    tick(&mut runtime);
    call(
        &mut runtime,
        "revise",
        json!({
            "id":source["id"],"expected_revision":1,
            "body":"The implementation language is Go.","idempotency_key":"change-source"
        }),
    );
    assert!(runtime
        .execute(NS, "inspect", json!({"id":recipe["output_id"]}))
        .is_err());
    tick(&mut runtime);
    let repaired = call(&mut runtime, "inspect", json!({"id":recipe["output_id"]}));
    assert_eq!(repaired["id"], first["id"]);
    assert_eq!(repaired["revision"], 2);
    assert!(repaired["body"].as_str().unwrap().contains("Go"));
    assert!(!repaired["body"].as_str().unwrap().contains("Rust"));
    tick(&mut runtime);
    let before_withdrawal = peer.calls();
    call(
        &mut runtime,
        "retract",
        json!({"id":source["id"],"expected_revision":2,"idempotency_key":"withdraw"}),
    );
    tick(&mut runtime);
    assert_eq!(peer.calls(), before_withdrawal);
    assert!(runtime
        .execute(
            NS,
            "inspect",
            json!({"id":recipe["output_id"],"history":true}),
        )
        .is_err());
}

#[test]
fn automatic_discovery_does_not_summarize_its_own_outputs_repeatedly() {
    let peer = Peer::new(reply);
    let (_directory, mut runtime) = database(Some(peer.config(false, true)));
    for index in 0..4 {
        remember(
            &mut runtime,
            &format!("source-{index}"),
            &format!("Synthetic observation number {index}."),
        );
    }
    let args = json!({
        "max_embeddings":0,"max_summaries":1,"auto_summary_min":4,
        "strategy":"fixed","max_group_chars":12000,"max_summary_chars":2000
    });
    call(&mut runtime, "maintain", args.clone());
    assert_eq!(peer.calls(), 1);
    let snapshot = call(&mut runtime, "export", json!({}));
    let summaries: Vec<_> = snapshot["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["kind"] == "summary")
        .collect();
    assert_eq!(summaries.len(), 1);
    call(&mut runtime, "maintain", args.clone());
    call(&mut runtime, "maintain", args);
    assert_eq!(
        peer.calls(),
        1,
        "published summaries are not fresh raw input"
    );
}

#[test]
fn withdrawn_automatic_recipe_is_not_reported_as_newly_queued() {
    let peer = Peer::new(reply);
    let (_directory, mut runtime) = database(Some(peer.config(false, true)));
    for index in 0..4 {
        remember(
            &mut runtime,
            &format!("source-{index}"),
            &format!("Persistent raw observation {index}."),
        );
    }
    let args = json!({
        "max_embeddings":0,"max_summaries":1,"auto_summary_min":4,
        "strategy":"fixed","max_group_chars":12000,"max_summary_chars":2000
    });
    call(&mut runtime, "maintain", args.clone());
    let snapshot = call(&mut runtime, "export", json!({}));
    let summary = snapshot["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["kind"] == "summary")
        .unwrap();
    call(
        &mut runtime,
        "retract",
        json!({"id":summary["id"],"expected_revision":1,"idempotency_key":"withdraw-summary"}),
    );
    let next = call(&mut runtime, "maintain", args);
    assert!(
        next["actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|action| action["kind"] != "summary_queued"),
        "a replayed historical receipt is not new queued work"
    );
    assert_eq!(peer.calls(), 1);
    let status = call(&mut runtime, "maintenance_status", json!({}));
    assert_eq!(status["total"], 1);
    assert_eq!(status["counts"]["cancelled"], 1);
}

#[test]
fn queued_summary_survives_runtime_restart_with_the_same_provider_identity() {
    let peer = Peer::new(reply);
    let config = peer.config(false, true);
    let (directory, mut runtime) = database(Some(config.clone()));
    let source = remember(
        &mut runtime,
        "source",
        "A restart must preserve pending work.",
    );
    let recipe = call(
        &mut runtime,
        "summarize",
        json!({"inputs":[source["id"]],"idempotency_key":"summary"}),
    );
    assert_eq!(peer.calls(), 0, "enqueueing does not run a model");
    drop(runtime);
    let store = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    let mut reopened = Runtime::new(store, Some(config)).unwrap();
    tick(&mut reopened);
    assert_eq!(peer.calls(), 1);
    let summary = call(&mut reopened, "inspect", json!({"id":recipe["output_id"]}));
    assert!(summary["body"].as_str().unwrap().contains("restart"));
}

#[test]
fn an_inflight_summary_cannot_publish_after_a_parallel_source_revision() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let peer = Peer::new(move |request| {
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        reply(request)
    });
    let (directory, mut runtime) = database(Some(peer.config(false, true)));
    let source = remember(&mut runtime, "source", "outdated source marker");
    let recipe = call(
        &mut runtime,
        "summarize",
        json!({"inputs":[source["id"]],"idempotency_key":"summary"}),
    );
    let handle = thread::spawn(move || {
        let result = runtime.execute(
            NS,
            "maintain",
            json!({"max_embeddings":0,"max_summaries":1,"auto_summary_min":0}),
        );
        (runtime, result)
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut writer = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    writer
        .execute(
            NS,
            "revise",
            json!({
                "id":source["id"],"expected_revision":1,"body":"replacement source marker",
                "idempotency_key":"parallel-revision"
            }),
        )
        .unwrap();
    release_tx.send(()).unwrap();
    let (mut runtime, _) = handle.join().unwrap();
    assert!(runtime
        .execute(NS, "inspect", json!({"id":recipe["output_id"]}))
        .is_err());
    assert!(!call(&mut runtime, "recall", json!({}))
        .to_string()
        .contains("outdated source marker"));
    assert_eq!(
        peer.calls(),
        1,
        "a rejected completion does not retry in a loop"
    );
}

#[test]
fn changing_summary_model_identity_does_not_recompute_an_old_recipe_silently() {
    let peer = Peer::new(reply);
    let mut config = peer.config(false, true);
    let (directory, mut runtime) = database(Some(config.clone()));
    let source = remember(&mut runtime, "source", "Pinned provider identity.");
    let recipe = call(
        &mut runtime,
        "summarize",
        json!({"inputs":[source["id"]],"idempotency_key":"summary"}),
    );
    drop(runtime);
    config.summary.as_mut().unwrap().revision = "different-served-weights".into();
    let store = Store::open(directory.path().join("memory.sqlite3")).unwrap();
    let mut changed = Runtime::new(store, Some(config)).unwrap();
    let _ = changed.execute(
        NS,
        "maintain",
        json!({"max_embeddings":0,"max_summaries":1,"auto_summary_min":0}),
    );
    assert_eq!(
        peer.calls(),
        0,
        "old recipe is pinned to its old processor key"
    );
    assert!(changed
        .execute(NS, "inspect", json!({"id":recipe["output_id"]}))
        .is_err());
}

#[test]
fn hybrid_model_failure_is_reported_and_explicit_vector_mode_fails() {
    let peer = Peer::new(|_| json!({"error":{"message":"private provider error body"}}));
    let (_directory, mut runtime) = database(Some(peer.config(true, false)));
    let source = remember(&mut runtime, "source", "orchard repair instructions");
    let degraded = call(
        &mut runtime,
        "recall",
        json!({"query":"orchard","mode":"hybrid"}),
    );
    assert_eq!(degraded["items"][0]["id"], source["id"]);
    assert_eq!(degraded["coverage"]["degraded"], true);
    assert!(!degraded.to_string().contains("private provider error body"));
    let error = runtime
        .execute(NS, "recall", json!({"query":"orchard","mode":"vector"}))
        .unwrap_err();
    assert!(!error.to_string().contains("private provider error body"));
    assert_eq!(peer.calls(), 2);
}

#[test]
fn missing_providers_fail_explicitly_without_disabling_lexical_memory() {
    let (_directory, mut runtime) = database(None);
    let source = remember(&mut runtime, "source", "offline lexical memory");
    assert_eq!(
        call(&mut runtime, "recall", json!({"query":"offline"}))["items"][0]["id"],
        source["id"]
    );
    for (method, args) in [
        ("recall", json!({"query":"offline","mode":"vector"})),
        (
            "summarize",
            json!({"inputs":[source["id"]],"idempotency_key":"summary"}),
        ),
        ("maintain", json!({})),
    ] {
        let error = runtime.execute(NS, method, args).unwrap_err();
        assert_eq!(error.code, "model_not_configured");
    }
}
