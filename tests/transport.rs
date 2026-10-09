#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    _dir: tempfile::TempDir,
    db: PathBuf,
    socket: PathBuf,
}

impl Daemon {
    fn start() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("rl-test-")
            .tempdir_in("/tmp")
            .unwrap();
        let db = dir.path().join("memory.db");
        let socket = dir.path().join("s");
        let mut child = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
            .arg("--db")
            .arg(&db)
            .args(["--namespace", "test", "serve", "--socket"])
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&socket).is_err() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "daemon exited during startup"
            );
            assert!(Instant::now() < deadline, "daemon startup timeout");
            std::thread::sleep(Duration::from_millis(20));
        }
        Self {
            child,
            _dir: dir,
            db,
            socket,
        }
    }

    fn call(&self, method: &str, params: Value) -> Value {
        exchange(
            &self.socket,
            json!({"id": 1, "method": method, "params": params}),
        )
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn connect(path: &Path) -> UnixStream {
    let stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
}

fn exchange(path: &Path, request: Value) -> Value {
    let mut stream = connect(path);
    writeln!(stream, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn socket_is_private_and_parallel_clients_share_pinned_namespace() {
    let daemon = Daemon::start();
    assert_eq!(
        std::fs::metadata(&daemon.socket)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let workers: Vec<_> = (0..8).map(|index| {
        let socket = daemon.socket.clone();
        std::thread::spawn(move || exchange(&socket, json!({
            "id": index, "method": "remember",
            "params": {"body": format!("record {index}"), "idempotency_key": format!("test-{index}")}
        })))
    }).collect();
    for worker in workers {
        assert!(worker.join().unwrap().get("result").is_some());
    }
    let recall = daemon.call("recall", json!({"limit": 20}));
    assert_eq!(recall["result"]["items"].as_array().unwrap().len(), 8);
    let attempt = exchange(
        &daemon.socket,
        json!({"id": 1, "method": "recall", "namespace": "other"}),
    );
    assert_eq!(attempt["error"]["code"], "invalid_request");
    let attempt = daemon.call("recall", json!({"namespace": "other"}));
    assert_eq!(attempt["error"]["code"], "invalid_request");
}

#[test]
fn malformed_and_oversized_clients_do_not_kill_daemon() {
    let daemon = Daemon::start();
    let mut stream = connect(&daemon.socket);
    stream.write_all(b"{bad}\n").unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["error"]["code"], "invalid_json");
    writeln!(
        stream,
        "{}",
        json!({"id": "next", "method": "status", "params": {}})
    )
    .unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], "next");
    assert!(response.get("result").is_some());
    let mut oversized = connect(&daemon.socket);
    let _ = oversized.write_all(&vec![b'x'; 1024 * 1024 + 1]);
    let mut line = String::new();
    BufReader::new(oversized).read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["error"]["code"], "message_too_large");
    assert!(daemon.call("status", json!({})).get("result").is_some());
}

#[test]
fn database_daemon_lock_is_enforced_across_socket_names() {
    let daemon = Daemon::start();
    let output = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
        .arg("--db")
        .arg(&daemon.db)
        .args(["serve", "--socket"])
        .arg(daemon.socket.with_file_name("other.sock"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("daemon_lock"));
}

#[test]
fn lost_socket_receipt_can_be_recovered_without_duplicate_write() {
    let daemon = Daemon::start();
    let params =
        json!({"body": "persisted before response loss", "idempotency_key": "lost-receipt"});
    let mut stream = connect(&daemon.socket);
    writeln!(
        stream,
        "{}",
        json!({"id": 41, "method": "remember", "params": params})
    )
    .unwrap();
    // Do not consume the original reply. Confirm through a separate connection
    // that the transaction committed before discarding the original connection.
    // A disconnect before confirmation can also abandon a request before commit;
    // that is a different allowed outcome, not a lost-committed-receipt failure.
    let deadline = Instant::now() + Duration::from_secs(10);
    let receipt = loop {
        let recovered = daemon.call(
            "operation",
            json!({"method": "remember", "idempotency_key": "lost-receipt"}),
        );
        if let Some(result) = recovered.get("result") {
            break result.clone();
        }
        assert!(Instant::now() < deadline, "receipt was not recovered");
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(stream);
    let replay = daemon.call("remember", params);
    assert_eq!(replay["result"], receipt);
    assert_eq!(
        daemon.call("recall", json!({}))["result"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn restart_recovers_stale_socket_and_persistent_database() {
    let mut daemon = Daemon::start();
    let receipt = daemon.call(
        "remember",
        json!({"body": "survives restart", "idempotency_key": "before-restart"}),
    );
    assert!(receipt.get("result").is_some());
    daemon.child.kill().unwrap();
    daemon.child.wait().unwrap();
    assert!(
        daemon.socket.exists(),
        "abrupt exit should leave a stale socket"
    );
    daemon.child = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
        .arg("--db")
        .arg(&daemon.db)
        .args(["--namespace", "test", "serve", "--socket"])
        .arg(&daemon.socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while UnixStream::connect(&daemon.socket).is_err() {
        assert!(daemon.child.try_wait().unwrap().is_none(), "restart failed");
        assert!(Instant::now() < deadline, "restart timeout");
        std::thread::sleep(Duration::from_millis(10));
    }
    let restored = daemon.call("inspect", json!({"id": receipt["result"]["id"]}));
    assert_eq!(restored["result"]["body"], "survives restart");
}

#[test]
fn mcp_end_to_end_preserves_core_errors_and_has_clean_stdout() {
    let daemon = Daemon::start();
    let mut child = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
        .args(["mcp", "--socket"])
        .arg(&daemon.socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"integration","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"remember","arguments":{"body":"Rust is selected","idempotency_key":"mcp-turn-1"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"inspect","arguments":{"id":"missing"}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"publish","arguments":{}}}),
    ];
    {
        let stdin = child.stdin.as_mut().unwrap();
        for message in messages {
            writeln!(stdin, "{message}").unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdout is MCP JSON only"))
        .collect();
    assert_eq!(responses.len(), 4);
    assert_eq!(responses[1]["result"]["isError"], false);
    assert_eq!(responses[2]["result"]["isError"], true);
    assert_eq!(
        responses[2]["result"]["structuredContent"]["error"]["code"],
        "not_found"
    );
    assert_eq!(responses[3]["error"]["code"], -32602);
}

#[test]
fn oversized_ipc_id_is_rejected_before_mutation() {
    let daemon = Daemon::start();
    // 86 Unicode scalar values, but 258 UTF-8 bytes.
    let invalid = exchange(
        &daemon.socket,
        json!({
            "id": "汉".repeat(86),
            "method": "remember",
            "params": {"body": "must never commit", "idempotency_key": "invalid-ipc-id"}
        }),
    );
    assert_eq!(invalid["id"], Value::Null);
    assert_eq!(invalid["error"]["code"], "invalid_request");
    let operation = daemon.call(
        "operation",
        json!({"method": "remember", "idempotency_key": "invalid-ipc-id"}),
    );
    assert_eq!(operation["error"]["code"], "not_found");
}

#[test]
fn oversized_mcp_id_cannot_commit_and_utf8_boundary_is_accepted() {
    let daemon = Daemon::start();
    let mut child = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
        .args(["mcp", "--socket"])
        .arg(&daemon.socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let boundary_id = "é".repeat(128); // Exactly 256 UTF-8 bytes.
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"id-boundary","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({
            "jsonrpc": "2.0",
            "id": "x".repeat(1024 * 1024 - 2048),
            "method": "tools/call",
            "params": {"name": "remember", "arguments": {
                "body": "must never commit", "idempotency_key": "invalid-mcp-id"
            }}
        }),
        json!({
            "jsonrpc": "2.0", "id": boundary_id,
            "method": "tools/call",
            "params": {"name": "remember", "arguments": {
                "body": "accepted bounded id", "idempotency_key": "valid-mcp-id"
            }}
        }),
    ];
    {
        let stdin = child.stdin.as_mut().unwrap();
        for message in messages {
            writeln!(stdin, "{message}").unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[1]["id"], Value::Null);
    assert_eq!(responses[1]["error"]["code"], -32600);
    assert_eq!(responses[2]["id"], boundary_id);
    assert_eq!(responses[2]["result"]["isError"], false);
    let absent = daemon.call(
        "operation",
        json!({"method": "remember", "idempotency_key": "invalid-mcp-id"}),
    );
    assert_eq!(absent["error"]["code"], "not_found");
    let committed = daemon.call(
        "operation",
        json!({"method": "remember", "idempotency_key": "valid-mcp-id"}),
    );
    assert!(committed.get("result").is_some());
}
