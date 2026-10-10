#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    _dir: tempfile::TempDir,
    db: PathBuf,
    socket: PathBuf,
}

impl Daemon {
    fn start() -> Self {
        Self::configured(None, &[])
    }

    fn configured(models: Option<Value>, extra_args: &[&str]) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("rl-test-")
            .tempdir_in("/tmp")
            .unwrap();
        let db = dir.path().join("memory.db");
        let socket = dir.path().join("s");
        let mut command = Command::new(env!("CARGO_BIN_EXE_recall-ledger"));
        command.arg("--db").arg(&db).args(["--namespace", "test"]);
        if let Some(models) = models {
            let path = dir.path().join("models.json");
            std::fs::write(&path, models.to_string()).unwrap();
            command.arg("--models").arg(path);
        }
        let mut child = command
            .args(["serve", "--socket"])
            .arg(&socket)
            .args(extra_args)
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
        .args(["--models", "/does-not-exist/mcp-does-not-load-models.json"])
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
        json!({"jsonrpc":"2.0","id":5,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"maintenance_status","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"recall","arguments":{"query":"Rust","mode":"vector"}}}),
        json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"summarize","arguments":{"inputs":["missing"],"idempotency_key":"mcp-summary"}}}),
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
    assert_eq!(responses.len(), 8);
    assert_eq!(responses[1]["result"]["isError"], false);
    assert_eq!(responses[2]["result"]["isError"], true);
    assert_eq!(
        responses[2]["result"]["structuredContent"]["error"]["code"],
        "not_found"
    );
    assert_eq!(responses[3]["error"]["code"], -32602);
    let tools = responses[4]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 7);
    let recall = tools.iter().find(|tool| tool["name"] == "recall").unwrap();
    assert_eq!(
        recall["inputSchema"]["properties"]["mode"]["enum"],
        json!(["lexical", "vector", "hybrid"])
    );
    assert_eq!(responses[5]["result"]["isError"], false);
    for response in &responses[6..] {
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["structuredContent"]["error"]["code"],
            "model_not_configured"
        );
    }
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

#[test]
fn daemon_refuses_client_maintenance_and_invalid_worker_options_fail_before_bind() {
    let daemon = Daemon::start();
    let rejected = daemon.call("maintain", json!({}));
    assert_eq!(rejected["error"]["code"], "unsupported_operation");
    assert!(daemon.call("status", json!({})).get("result").is_some());
    for (option, value) in [
        ("--worker-interval-ms", "99"),
        ("--auto-summary-min", "65"),
        ("--max-group-chars", "16001"),
        ("--max-summary-chars", "0"),
        ("--summary-strategy", "unsupported"),
    ] {
        let socket = daemon.socket.with_file_name("invalid.sock");
        let output = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
            .arg("--db")
            .arg(daemon.db.with_file_name("invalid.db"))
            .args(["serve", "--socket"])
            .arg(&socket)
            .args([option, value])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{option} should be rejected");
        assert!(!socket.exists());
    }
}

/// Deliberately fixed protocol fixture; it tests worker plumbing, not LLM quality.
struct SummaryStub {
    url: String,
    count: Arc<AtomicUsize>,
    embedding_count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn stub_request(stream: &TcpStream) -> std::io::Result<Option<Value>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut length = None;
    let mut header_bytes = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        // A connecting client may close without sending a request.
        if read == 0 {
            return Ok(None);
        }
        header_bytes += read;
        if header_bytes > 16_384 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "fixture request headers exceed limit",
            ));
        }
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "fixture request has invalid content length",
                    )
                })?);
            }
        }
    }
    let length = length.filter(|length| *length <= 262_144).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "fixture request is missing a bounded content length",
        )
    })?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn disconnected(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
    )
}

impl SummaryStub {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        listener.set_nonblocking(true).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let embedding_count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_count = count.clone();
        let thread_embedding_count = embedding_count.clone();
        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fixture listener failed: {error}"),
                };
                // Darwin can inherit O_NONBLOCK from the listener. Explicitly
                // restore blocking reads before waiting for HTTP request bytes.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = match stub_request(&stream) {
                    Ok(Some(request)) => request,
                    Ok(None) => continue,
                    Err(error) if disconnected(&error) => continue,
                    Err(error) => panic!("fixture request failed: {error}"),
                };
                let response = if let Some(input) = request["input"].as_str() {
                    let embedding = if input.contains("June") {
                        json!([0.0, 1.0])
                    } else {
                        json!([1.0, 0.0])
                    };
                    thread_embedding_count.fetch_add(1, Ordering::SeqCst);
                    json!({"model":"embedding-stub","data":[{"index":0,"embedding":embedding}]})
                } else {
                    let content = request["messages"][1]["content"].as_str().unwrap();
                    let summary = if content.contains("June") {
                        "Release June"
                    } else {
                        "Release May"
                    };
                    thread_count.fetch_add(1, Ordering::SeqCst);
                    json!({
                        "model":"summary-stub",
                        "choices":[{"finish_reason":"stop","message":{"role":"assistant","content":summary}}]
                    })
                }
                .to_string();
                let written = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(), response
                );
                if let Err(error) = written {
                    if !disconnected(&error) {
                        panic!("fixture response failed: {error}");
                    }
                }
            }
        });
        Self {
            url,
            count,
            embedding_count,
            stop,
            thread: Some(thread),
        }
    }
    fn config(&self) -> Value {
        json!({"summary":{
            "url":self.url,"model":"summary-stub","revision":"fixture-v1","timeout_seconds":3
        }})
    }
    fn full_config(&self) -> Value {
        let mut config = self.config();
        config["embedding"] = json!({
            "url":self.url.replace("/chat/completions","/embeddings"),
            "model":"embedding-stub","revision":"fixture-v1",
            "dimensions":2,"timeout_seconds":3
        });
        config
    }
}
impl Drop for SummaryStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let result = self.thread.take().unwrap().join();
        // Preserve a primary test failure instead of aborting during unwinding.
        // An otherwise successful test still fails if its fixture thread panicked.
        if !std::thread::panicking() {
            assert!(result.is_ok(), "summary fixture thread panicked");
        }
    }
}

#[test]
fn http_fixture_accepts_delayed_request_bytes_and_idle_disconnects() {
    let provider = SummaryStub::new();
    let address = provider
        .url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    drop(TcpStream::connect(address).unwrap());
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    // Let accept happen before any request byte is available, reproducing the
    // nonblocking-read race independently of the model HTTP client's timing.
    std::thread::sleep(Duration::from_millis(100));
    let request = json!({"model":"embedding-stub","input":"June"}).to_string();
    write!(
        stream,
        "POST /v1/embeddings HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{request}",
        request.len()
    ).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    let body: Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["data"][0]["embedding"], json!([0.0, 1.0]));
    assert_eq!(provider.embedding_count.load(Ordering::SeqCst), 1);
}

#[test]
fn configured_daemon_worker_automatically_produces_and_repairs_a_summary() {
    let provider = SummaryStub::new();
    let daemon = Daemon::configured(
        Some(provider.full_config()),
        &["--worker-interval-ms", "100", "--auto-summary-min", "0"],
    );
    let source = daemon.call(
        "remember",
        json!({"body":"Release date is May.","idempotency_key":"daemon-source"}),
    );
    let queued = daemon.call(
        "summarize",
        json!({"inputs":[source["result"]["id"]],"idempotency_key":"daemon-summary"}),
    );
    let output_id = queued["result"]["output_id"].clone();
    assert!(output_id.is_string(), "{queued}");
    let wait_for = |revision: i64| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let response = daemon.call("inspect", json!({"id":output_id}));
            if response["result"]["revision"] == revision {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "summary worker timeout: {response}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert_eq!(wait_for(1)["result"]["body"], "Release May");
    let changed = daemon.call(
        "revise",
        json!({
            "id":source["result"]["id"],"expected_revision":1,"body":"Release date is June.",
            "idempotency_key":"daemon-source-new"
        }),
    );
    assert!(changed.get("result").is_some(), "{changed}");
    assert_eq!(wait_for(2)["result"]["body"], "Release June");
    assert_eq!(provider.count.load(Ordering::SeqCst), 2);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let recall = daemon.call("recall", json!({"query":"June","mode":"vector"}));
        let items = recall["result"]["items"].as_array().unwrap();
        if items.iter().any(|record| record["id"] == output_id) {
            assert!(items
                .iter()
                .all(|record| !record["body"].as_str().unwrap().contains("May")));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "automatic embedding timeout: {recall}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(provider.embedding_count.load(Ordering::SeqCst) >= 3);
}

#[test]
fn no_worker_preserves_queued_work_and_trusted_cli_maintain_processes_it() {
    let provider = SummaryStub::new();
    let daemon = Daemon::configured(
        Some(provider.config()),
        &[
            "--no-worker",
            "--worker-interval-ms",
            "100",
            "--auto-summary-min",
            "0",
        ],
    );
    let source = daemon.call(
        "remember",
        json!({"body":"Release date is May.","idempotency_key":"manual-source"}),
    );
    let queued = daemon.call(
        "summarize",
        json!({
            "inputs":[source["result"]["id"]],"idempotency_key":"manual-summary"
        }),
    );
    assert!(queued.get("result").is_some(), "{queued}");
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(provider.count.load(Ordering::SeqCst), 0);
    let output = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
        .arg("--db")
        .arg(&daemon.db)
        .args(["--namespace", "test", "--models"])
        .arg(daemon.db.with_file_name("models.json"))
        .args([
            "maintain",
            "--max-embeddings",
            "0",
            "--max-summaries",
            "1",
            "--auto-summary-min",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result.is_object());
    let read = daemon.call("inspect", json!({"id":queued["result"]["output_id"]}));
    assert_eq!(read["result"]["body"], "Release May");
    assert_eq!(provider.count.load(Ordering::SeqCst), 1);
}
