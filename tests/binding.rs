//! Destination assertions use synthetic databases/daemons, never host accounts.
#![cfg(unix)]

use recall_ledger::models::{Config, EmbeddingConfig};
use recall_ledger::runtime::Runtime;
use recall_ledger::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Daemon {
    fn start(database: &Path, socket: &Path, namespace: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
            .arg("--db")
            .arg(database)
            .args(["--namespace", namespace, "serve", "--socket"])
            .arg(socket)
            .arg("--no-worker")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(socket).is_err() {
            assert!(child.try_wait().unwrap().is_none(), "daemon exited");
            assert!(Instant::now() < deadline, "daemon readiness deadline");
            thread::sleep(Duration::from_millis(10));
        }
        Self {
            child,
            socket: socket.to_owned(),
        }
    }

    fn call(&self, method: &str, params: Value, expected: Option<&Value>) -> Value {
        let stream = UnixStream::connect(&self.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        exchange(&mut BufReader::new(stream), method, params, expected)
    }

    fn binding(&self) -> Value {
        self.call("binding", json!({}), None)["result"].clone()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn exchange(
    stream: &mut BufReader<UnixStream>,
    method: &str,
    params: Value,
    expected: Option<&Value>,
) -> Value {
    let mut request = json!({"id":7,"method":method,"params":params});
    if let Some(expected) = expected {
        request["expected_binding"] = expected.clone();
    }
    writeln!(stream.get_mut(), "{request}").unwrap();
    let mut line = String::new();
    stream.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn mismatch(response: &Value, forbidden: &str) {
    assert_eq!(response["error"]["code"], "binding_mismatch", "{response}");
    assert_eq!(response["error"]["retryable"], false);
    assert!(response.get("result").is_none());
    assert!(!response.to_string().contains(forbidden));
}

fn fixture() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rl-bind-")
        .tempdir_in("/tmp")
        .unwrap()
}

#[test]
fn binding_is_body_free_and_matches_the_existing_history_identity() {
    let temp = fixture();
    let daemon = Daemon::start(&temp.path().join("db"), &temp.path().join("s"), "alpha");
    let identity = daemon.binding();
    assert_eq!(identity.as_object().unwrap().len(), 2);
    assert_eq!(identity["namespace"], "alpha");
    uuid::Uuid::parse_str(identity["history_id"].as_str().unwrap()).unwrap();
    let status = daemon.call("status", json!({}), Some(&identity));
    assert_eq!(
        status["result"]["commit_token"]
            .as_str()
            .unwrap()
            .split(':')
            .next()
            .unwrap(),
        identity["history_id"].as_str().unwrap()
    );
    let bound = daemon.call("binding", json!({}), Some(&identity));
    assert_eq!(bound["result"], identity);
}

#[test]
fn same_socket_new_namespace_rejects_before_bodies_receipts_and_mutations() {
    let temp = fixture();
    let database = temp.path().join("db");
    let socket = temp.path().join("s");
    let first = Daemon::start(&database, &socket, "alpha");
    let pinned = first.binding();
    drop(first);
    let second = Daemon::start(&database, &socket, "beta");
    assert_eq!(pinned["history_id"], second.binding()["history_id"]);
    let secret = "BETA-SYNTHETIC-CONTENT";
    let seed_params = json!({"body":secret,"idempotency_key":"shared-key"});
    let seed = second.call("remember", seed_params.clone(), None)["result"].clone();
    let before = second.call("status", json!({}), None)["result"].clone();
    for (method, params) in [
        ("recall", json!({})),
        ("inspect", json!({"id":seed["id"],"history":true})),
        ("export", json!({})),
        ("status", json!({})),
        ("binding", json!({})),
        ("changes", json!({})),
        (
            "operation",
            json!({"method":"remember","idempotency_key":"shared-key"}),
        ),
        // Even an already committed identical key cannot bypass the assertion.
        ("remember", seed_params),
        (
            "remember",
            json!({"body":"ALPHA QUEUED WRITE","idempotency_key":"new-key"}),
        ),
        (
            "candidate_submit",
            json!({"body":"ALPHA OFFLINE QUEUED CANDIDATE","event_id":"offline-event",
                "origin":"user_message","source":"fixture","idempotency_key":"offline-key"}),
        ),
        // Binding failure wins before a configured provider is even required.
        (
            "summarize",
            json!({"inputs":[seed["id"]],"idempotency_key":"bad-summary"}),
        ),
    ] {
        mismatch(&second.call(method, params, Some(&pinned)), secret);
    }
    let after = second.call("status", json!({}), None)["result"].clone();
    assert_eq!(before["commit_token"], after["commit_token"]);
    assert_eq!(after["records"], 1);
    assert!(
        second.call("candidate_list", json!({}), None)["result"]["candidates"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn same_socket_same_namespace_different_database_rejects_old_binding() {
    let temp = fixture();
    let socket = temp.path().join("s");
    let first = Daemon::start(&temp.path().join("first.db"), &socket, "alpha");
    let pinned = first.binding();
    drop(first);
    let second = Daemon::start(&temp.path().join("second.db"), &socket, "alpha");
    assert_ne!(pinned["history_id"], second.binding()["history_id"]);
    let seed = second.call(
        "remember",
        json!({"body":"OTHER DATABASE BODY","idempotency_key":"seed"}),
        None,
    )["result"]
        .clone();
    mismatch(
        &second.call("inspect", json!({"id":seed["id"]}), Some(&pinned)),
        "OTHER DATABASE BODY",
    );
    mismatch(
        &second.call(
            "remember",
            json!({"body":"misdirected","idempotency_key":"offline"}),
            Some(&pinned),
        ),
        "OTHER DATABASE BODY",
    );
    assert_eq!(
        second.call("status", json!({}), None)["result"]["commit_token"],
        seed["commit_token"]
    );
}

#[test]
fn ordinary_restart_keeps_binding_and_idempotent_recovery() {
    let temp = fixture();
    let database = temp.path().join("db");
    let socket = temp.path().join("s");
    let first = Daemon::start(&database, &socket, "alpha");
    let pinned = first.binding();
    let params = json!({"body":"SYNTHETIC RESTART BODY","idempotency_key":"restart"});
    let receipt = first.call("remember", params.clone(), Some(&pinned))["result"].clone();
    drop(first);
    let second = Daemon::start(&database, &socket, "alpha");
    assert_eq!(second.binding(), pinned);
    assert_eq!(
        second.call("remember", params, Some(&pinned))["result"],
        receipt
    );
    assert_eq!(
        second.call(
            "operation",
            json!({"method":"remember","idempotency_key":"restart"}),
            Some(&pinned)
        )["result"],
        receipt
    );
    let recall = second.call(
        "recall",
        json!({"minimum_commit":receipt["commit_token"]}),
        Some(&pinned),
    );
    assert_eq!(recall["result"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        recall["result"]["items"][0]["body"],
        "SYNTHETIC RESTART BODY"
    );
}

#[test]
fn restore_requires_explicit_new_binding_even_when_record_ids_survive() {
    let temp = fixture();
    let live = temp.path().join("live.db");
    let backup = temp.path().join("backup.db");
    let restored = temp.path().join("restored.db");
    let socket = temp.path().join("s");
    let first = Daemon::start(&live, &socket, "alpha");
    let pinned = first.binding();
    let seed = first.call(
        "remember",
        json!({"body":"SYNTHETIC BACKUP BODY","idempotency_key":"seed"}),
        Some(&pinned),
    )["result"]
        .clone();
    Store::open(&live).unwrap().backup(&backup).unwrap();
    drop(first);
    Store::restore(&backup, &restored).unwrap();
    let second = Daemon::start(&restored, &socket, "alpha");
    let restored_binding = second.binding();
    assert_ne!(pinned, restored_binding);
    mismatch(
        &second.call("inspect", json!({"id":seed["id"]}), Some(&pinned)),
        "SYNTHETIC BACKUP BODY",
    );
    mismatch(
        &second.call(
            "recall",
            json!({"minimum_commit":seed["commit_token"]}),
            Some(&pinned),
        ),
        "SYNTHETIC BACKUP BODY",
    );
    assert_eq!(
        second.call("inspect", json!({"id":seed["id"]}), Some(&restored_binding))["result"]["body"],
        "SYNTHETIC BACKUP BODY"
    );
    // An explicit new binding does not silently discard a caller's old barrier.
    assert_eq!(
        second.call(
            "recall",
            json!({"minimum_commit":seed["commit_token"]}),
            Some(&restored_binding)
        )["error"]["code"],
        "commit_history_mismatch"
    );
}

#[test]
fn malformed_binding_never_degrades_to_unbound_execution() {
    let temp = fixture();
    let daemon = Daemon::start(&temp.path().join("db"), &temp.path().join("s"), "alpha");
    let binding = daemon.binding();
    for invalid in [
        Value::Null,
        json!({}),
        json!({"namespace":"alpha"}),
        json!({"namespace":"alpha","history_id":17}),
        json!({"namespace":"alpha","history_id":"not-a-uuid"}),
        json!({"namespace":"alpha\n","history_id":binding["history_id"]}),
        json!({"namespace":"alpha","history_id":binding["history_id"],"extra":true}),
    ] {
        let response = daemon.call(
            "remember",
            json!({"body":"must not be written","idempotency_key":"malformed"}),
            Some(&invalid),
        );
        assert_eq!(response["error"]["code"], "invalid_request", "{response}");
    }
    assert_eq!(
        daemon.call("status", json!({}), None)["result"]["records"],
        0
    );
}

#[test]
fn a_persistent_connection_checks_actual_history_on_every_request() {
    let temp = fixture();
    let database = temp.path().join("db");
    let daemon = Daemon::start(&database, &temp.path().join("s"), "alpha");
    let pinned = daemon.binding();
    let stream = UnixStream::connect(&daemon.socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut stream = BufReader::new(stream);
    assert!(exchange(&mut stream, "status", json!({}), Some(&pinned))
        .get("result")
        .is_some());
    // Fault injection changes identity on the very same SQLite connection's
    // database. A cached open-time/preflight binding would miss this change.
    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE meta SET value=?1 WHERE key='store_id'",
            [uuid::Uuid::new_v4().to_string()],
        )
        .unwrap();
    mismatch(
        &exchange(
            &mut stream,
            "remember",
            json!({"body":"must not be committed","idempotency_key":"changed-history"}),
            Some(&pinned),
        ),
        "must not be committed",
    );
    assert_eq!(
        daemon.call("status", json!({}), None)["result"]["records"],
        0
    );
}

#[test]
fn configured_recall_checks_again_after_synthetic_provider_work() {
    let temp = fixture();
    let database = temp.path().join("db");
    let mut store = Store::open(&database).unwrap();
    let pinned = store.execute("alpha", "binding", json!({})).unwrap();
    store
        .execute(
            "alpha",
            "remember",
            json!({"body":"SYNTHETIC STORED BODY","idempotency_key":"seed"}),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/embeddings", listener.local_addr().unwrap());
    let peer_database = database.clone();
    let peer = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 16384);
        }
        let text = String::from_utf8(headers).unwrap();
        let length: usize = text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .unwrap()
            .1
            .trim()
            .parse()
            .unwrap();
        assert!(length < 16384);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        Connection::open(peer_database)
            .unwrap()
            .execute(
                "UPDATE meta SET value=?1 WHERE key='store_id'",
                [uuid::Uuid::new_v4().to_string()],
            )
            .unwrap();
        // Synthetic HTTP fixture; no model inference is executed.
        let response = r#"{"data":[{"embedding":[1.0,0.0]}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .unwrap();
    });
    let mut runtime = Runtime::new(
        store,
        Some(Config {
            embedding: Some(EmbeddingConfig {
                url,
                model: "synthetic-fixture".into(),
                revision: "no-real-model".into(),
                dimensions: Some(2),
                task_field: None,
                timeout_seconds: 5,
            }),
            summary: None,
        }),
    )
    .unwrap();
    let error = runtime
        .execute_bound(
            "alpha",
            "recall",
            json!({"query":"SYNTHETIC","mode":"hybrid"}),
            Some(&pinned),
        )
        .unwrap_err();
    peer.join().unwrap();
    assert_eq!(error.code, "binding_mismatch");
    assert!(!error.message.contains("SYNTHETIC STORED BODY"));
    // A failed protected call must not accidentally poison the next operation.
    assert!(runtime.execute("alpha", "binding", json!({})).is_ok());
}
