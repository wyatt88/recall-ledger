//! Real stdio MCP processes and temporary daemons; no host account or model calls.
#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Daemon {
    fn start(database: &Path, socket: &Path, namespace: &str) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
            .arg("--db")
            .arg(database)
            .args(["--namespace", namespace, "serve", "--socket"])
            .arg(socket)
            .arg("--no-worker")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut daemon = Self {
            child,
            socket: socket.to_owned(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(socket).is_err() {
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "daemon exited during startup"
            );
            assert!(Instant::now() < deadline, "daemon readiness deadline");
            thread::sleep(Duration::from_millis(10));
        }
        daemon
    }

    fn call(&self, method: &str, params: Value) -> Value {
        let mut stream = UnixStream::connect(&self.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            json!({"id":1,"method":method,"params":params})
        )
        .unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }

    fn binding(&self) -> Value {
        self.call("binding", json!({}))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
}

impl Mcp {
    fn start(socket: &Path, binding_file: Option<&Path>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_recall-ledger"));
        command.args(["mcp", "--socket"]).arg(socket);
        if let Some(path) = binding_file {
            command.arg("--binding-file").arg(path);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let message = line
                    .map_err(|err| err.to_string())
                    .and_then(|line| serde_json::from_str(&line).map_err(|err| err.to_string()));
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        let mut mcp = Self {
            child,
            input,
            output,
            reader: Some(reader),
            next_id: 1,
        };
        let initialized = mcp.exchange(json!({
            "jsonrpc":"2.0","method":"initialize","params":{
                "protocolVersion":"2025-11-25","capabilities":{},
                "clientInfo":{"name":"binding-regression","version":"1"}
            }
        }));
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        mcp.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        mcp
    }

    fn send(&mut self, request: Value) {
        writeln!(self.input, "{request}").unwrap();
        self.input.flush().unwrap();
    }

    fn exchange(&mut self, mut request: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        request["id"] = json!(id);
        self.send(request);
        let response = self
            .output
            .recv_timeout(Duration::from_secs(10))
            .expect("MCP response deadline or unexpected process exit")
            .expect("MCP stdout must contain JSON only");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], id, "unexpected response or notification");
        response
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.exchange(json!({
            "jsonrpc":"2.0","method":"tools/call","params":{"name":name,"arguments":arguments}
        }))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn fixture() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rl-mcp-bind-")
        .tempdir_in("/tmp")
        .unwrap()
}

fn save_binding(daemon: &Daemon, directory: &Path) -> PathBuf {
    let output = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
        .args(["binding", "--socket"])
        .arg(&daemon.socket)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let binding: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(binding, daemon.binding());
    assert_eq!(binding.as_object().unwrap().len(), 2);
    let path = directory.join("binding.json");
    std::fs::write(&path, output.stdout).unwrap();
    path
}

fn success(response: Value) -> Value {
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], false, "{response}");
    let structured = response["result"]["structuredContent"].clone();
    let text: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(structured, text);
    structured
}

fn mismatch(response: Value, forbidden: &str) {
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], true, "{response}");
    let error = &response["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], "binding_mismatch", "{response}");
    assert_eq!(error["retryable"], false);
    assert!(!response.to_string().contains(forbidden), "{response}");
    let text: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, response["result"]["structuredContent"]);
}

fn refuses_foreign_reads_receipts_and_writes(mcp: &mut Mcp, daemon: &Daemon) {
    let secret = "SYNTHETIC-FOREIGN-MEMORY";
    let duplicate = json!({"body":secret,"idempotency_key":"foreign-seed"});
    let seed = daemon.call("remember", duplicate.clone());
    let before = daemon.call("status", json!({}));
    for (name, arguments) in [
        ("recall", json!({})),
        ("inspect", json!({"id":seed["id"],"history":true})),
        ("remember", duplicate),
        (
            "remember",
            json!({"body":"MISDIRECTED-SYNTHETIC-WRITE","idempotency_key":"new-write"}),
        ),
        (
            "candidate_submit",
            json!({"body":"MISDIRECTED-OFFLINE-CANDIDATE","event_id":"queued-event",
                "origin":"user_message","source":"test","idempotency_key":"queued-key"}),
        ),
    ] {
        mismatch(mcp.tool(name, arguments), secret);
    }
    // Client-supplied fields cannot override the process's saved assertion.
    let actual_binding = daemon.binding();
    mismatch(
        mcp.exchange(json!({
            "jsonrpc":"2.0","method":"tools/call","expected_binding":actual_binding,
            "params":{"name":"recall","arguments":{}}
        })),
        secret,
    );
    mismatch(
        mcp.tool("recall", json!({"expected_binding":actual_binding})),
        secret,
    );
    let after = daemon.call("status", json!({}));
    assert_eq!(after["commit_token"], before["commit_token"]);
    assert_eq!(after["records"], 1);
    assert!(daemon.call("candidate_list", json!({}))["candidates"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn saved_binding_refuses_namespace_replacement_during_live_mcp_session() {
    let temp = fixture();
    let database = temp.path().join("db");
    let socket = temp.path().join("s");
    let first = Daemon::start(&database, &socket, "alpha");
    let binding_file = save_binding(&first, temp.path());
    let mut mcp = Mcp::start(&socket, Some(&binding_file));
    success(mcp.tool("recall", json!({})));
    drop(first);
    let replacement = Daemon::start(&database, &socket, "beta");
    refuses_foreign_reads_receipts_and_writes(&mut mcp, &replacement);
}

#[test]
fn saved_binding_refuses_new_database_in_live_and_restarted_mcp_processes() {
    let temp = fixture();
    let socket = temp.path().join("s");
    let first = Daemon::start(&temp.path().join("first.db"), &socket, "alpha");
    let binding_file = save_binding(&first, temp.path());
    let old_binding = first.binding();
    let mut mcp = Mcp::start(&socket, Some(&binding_file));
    success(mcp.tool("recall", json!({})));
    drop(first);
    let replacement = Daemon::start(&temp.path().join("other.db"), &socket, "alpha");
    assert_ne!(old_binding, replacement.binding());
    refuses_foreign_reads_receipts_and_writes(&mut mcp, &replacement);
    drop(mcp);
    let mut restarted = Mcp::start(&socket, Some(&binding_file));
    refuses_foreign_reads_receipts_and_writes(&mut restarted, &replacement);
}

#[test]
fn ordinary_daemon_restart_preserves_mcp_reads_and_idempotent_receipts() {
    let temp = fixture();
    let database = temp.path().join("db");
    let socket = temp.path().join("s");
    let first = Daemon::start(&database, &socket, "alpha");
    let binding_file = save_binding(&first, temp.path());
    let mut mcp = Mcp::start(&socket, Some(&binding_file));
    let params = json!({"body":"SYNTHETIC-RESTART-MEMORY","idempotency_key":"restart"});
    let receipt = success(mcp.tool("remember", params.clone()));
    drop(first);
    let restarted = Daemon::start(&database, &socket, "alpha");
    assert_eq!(success(mcp.tool("remember", params)), receipt);
    let recalled = success(mcp.tool("recall", json!({"minimum_commit":receipt["commit_token"]})));
    assert_eq!(recalled["items"].as_array().unwrap().len(), 1);
    assert_eq!(recalled["items"][0]["body"], "SYNTHETIC-RESTART-MEMORY");
    assert_eq!(
        restarted.call("status", json!({}))["commit_token"],
        receipt["commit_token"]
    );
}

#[test]
fn no_binding_file_is_pinned_for_session_but_new_process_selects_current_daemon() {
    let temp = fixture();
    let socket = temp.path().join("s");
    let first = Daemon::start(&temp.path().join("first.db"), &socket, "alpha");
    let mut mcp = Mcp::start(&socket, None);
    success(mcp.tool("recall", json!({})));
    drop(first);
    let replacement = Daemon::start(&temp.path().join("other.db"), &socket, "beta");
    refuses_foreign_reads_receipts_and_writes(&mut mcp, &replacement);
    // Pinning across process restarts requires a saved binding file.
    let mut newly_selected = Mcp::start(&socket, None);
    let recalled = success(newly_selected.tool("recall", json!({})));
    assert_eq!(recalled["items"][0]["body"], "SYNTHETIC-FOREIGN-MEMORY");
}

#[test]
fn malformed_binding_files_fail_without_daemon_fallback_or_content_echo() {
    let temp = fixture();
    let daemon = Daemon::start(&temp.path().join("db"), &temp.path().join("s"), "alpha");
    let actual = daemon.binding();
    let before = daemon.call("status", json!({}));
    let marker = "SYNTHETIC-BINDING-SECRET";
    let malformed = [
        "{broken JSON SYNTHETIC-BINDING-SECRET".to_owned(),
        "null".to_owned(),
        json!({"namespace":"alpha"}).to_string(),
        json!({"namespace":"alpha","history_id":marker}).to_string(),
        json!({"namespace":"alpha","history_id":actual["history_id"],"extra":marker}).to_string(),
        json!({"namespace":12,"history_id":actual["history_id"]}).to_string(),
        format!("{}{}", " ".repeat(4096), marker),
    ];
    let mut paths = Vec::new();
    for (index, bytes) in malformed.iter().enumerate() {
        let path = temp.path().join(format!("bad-{index}.json"));
        std::fs::write(&path, bytes).unwrap();
        paths.push(path);
    }
    let valid = save_binding(&daemon, temp.path());
    let link = temp.path().join("symlink.json");
    std::os::unix::fs::symlink(valid, &link).unwrap();
    paths.extend([
        link,
        temp.path().to_owned(),
        temp.path().join("missing.json"),
    ]);
    for path in paths {
        let output = Command::new(env!("CARGO_BIN_EXE_recall-ledger"))
            .args(["mcp", "--socket"])
            .arg(&daemon.socket)
            .arg("--binding-file")
            .arg(path)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success(), "bad binding must stop startup");
        assert!(
            output.stdout.is_empty(),
            "no MCP session on invalid binding"
        );
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(matches!(
            error["error"]["code"].as_str(),
            Some("invalid_binding" | "invalid_request")
        ));
        assert!(!error.to_string().contains(marker));
    }
    assert_eq!(
        daemon.call("status", json!({}))["commit_token"],
        before["commit_token"]
    );
}

#[test]
fn tool_call_notifications_have_no_write_effect_in_bound_mcp_session() {
    let temp = fixture();
    let daemon = Daemon::start(&temp.path().join("db"), &temp.path().join("s"), "alpha");
    let binding_file = save_binding(&daemon, temp.path());
    let mut mcp = Mcp::start(&daemon.socket, Some(&binding_file));
    let before = daemon.call("status", json!({}));
    mcp.send(json!({
        "jsonrpc":"2.0","method":"tools/call",
        "params":{"name":"remember","arguments":{
            "body":"SYNTHETIC-NOTIFICATION-WRITE","idempotency_key":"notification"
        }}
    }));
    let ping = mcp.exchange(json!({"jsonrpc":"2.0","method":"ping"}));
    assert_eq!(ping["result"], json!({}));
    let after = daemon.call("status", json!({}));
    assert_eq!(after["commit_token"], before["commit_token"]);
    assert_eq!(after["records"], 0);
}
