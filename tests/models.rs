//! Loopback stub HTTP tests: these do not contact or run a model.

use recall_ledger::models::{Config, EmbeddingConfig, SummaryConfig};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

fn embedding_config(url: String) -> Config {
    Config {
        embedding: Some(EmbeddingConfig {
            url,
            model: "test-embedding".into(),
            revision: "weights-v1".into(),
            dimensions: Some(3),
            task_field: Some("input_type".into()),
            timeout_seconds: 2,
        }),
        summary: None,
    }
}

fn summary_config(url: String) -> Config {
    Config {
        embedding: None,
        summary: Some(SummaryConfig {
            url,
            model: "test-summary".into(),
            revision: "weights-v1".into(),
            timeout_seconds: 2,
            max_tokens: 128,
            extra_body: None,
        }),
    }
}

fn embedding_response(vector: Value) -> Value {
    json!({"model":"test-embedding","data":[{"index":0,"embedding":vector}]})
}

fn summary_response(text: &str) -> Value {
    json!({
        "model":"test-summary",
        "choices":[{"finish_reason":"stop","message":{"role":"assistant","content":text}}]
    })
}

/// Start one request/response exchange and capture exactly what was sent.
fn stub(
    status: &str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
) -> (String, Receiver<Value>, thread::JoinHandle<()>) {
    response_stub(status, headers, body, delay, Duration::ZERO)
}

fn response_stub(
    status: &str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay_headers: Duration,
    delay_body: Duration,
) -> (String, Receiver<Value>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let status = status.to_owned();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let end = loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 16_384, "excessive request headers");
            if request.ends_with(b"\r\n\r\n") {
                break request.len();
            }
        };
        let request_headers = String::from_utf8_lossy(&request[..end]);
        let content_length: usize = request_headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        assert!(content_length <= 262_144);
        let mut request_body = vec![0; content_length];
        stream.read_exact(&mut request_body).unwrap();
        let request_json: Value = serde_json::from_slice(&request_body).unwrap();
        sender.send(request_json).unwrap();
        thread::sleep(delay_headers);
        let mut response =
            format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\n");
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        // Timeout and over-limit tests intentionally close before we finish.
        let _ = stream.write_all(response.as_bytes());
        thread::sleep(delay_body);
        let _ = stream.write_all(&body);
    });
    (format!("http://{address}/v1/test"), receiver, handle)
}

fn json_stub(value: Value) -> (String, Receiver<Value>, thread::JoinHandle<()>) {
    let body = serde_json::to_vec(&value).unwrap();
    stub(
        "200 OK",
        vec![("Content-Length".into(), body.len().to_string())],
        body,
        Duration::ZERO,
    )
}

#[test]
fn model_config_is_bounded_strict_and_requires_revision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("models.json");
    let config = embedding_config("http://127.0.0.1:8080/v1/embeddings".into());
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert_eq!(
        Config::load(&path).unwrap().embedding_key().unwrap(),
        config.embedding_key().unwrap()
    );
    for value in [
        json!({"embedding":{"url":"http://127.0.0.1:8080","model":"m"}}),
        json!({"embedding":{"url":"http://127.0.0.1:8080","model":"m","revision":""}}),
        json!({"unknown":true}),
        json!({"summary":{"url":"http://127.0.0.1","model":"m","revision":"v1","extra_body":[]}}),
    ] {
        std::fs::write(&path, value.to_string()).unwrap();
        assert_eq!(Config::load(&path).unwrap_err().code, "model_config_error");
    }
    std::fs::write(&path, vec![b' '; 65_537]).unwrap();
    assert_eq!(Config::load(&path).unwrap_err().code, "model_config_error");
    assert!(Config::default().validate().is_ok());
    assert_eq!(
        Config::default().embedding_key().unwrap_err().code,
        "model_not_configured"
    );
    assert_eq!(
        Config::default().summary_key().unwrap_err().code,
        "model_not_configured"
    );
}

#[test]
fn provider_identity_changes_with_model_space_configuration() {
    let config = embedding_config("http://127.0.0.1:8080/v1/embeddings".into());
    let identity = config.embedding_key().unwrap();
    let original = serde_json::to_value(&config).unwrap();
    for (field, value) in [
        ("model", json!("another-model")),
        ("revision", json!("weights-v2")),
        ("dimensions", json!(2)),
        ("task_field", json!("task")),
        ("url", json!("http://127.0.0.1:8081/v1/embeddings")),
        ("timeout_seconds", json!(3)),
    ] {
        let mut changed = original.clone();
        changed["embedding"][field] = value;
        let changed: Config = serde_json::from_value(changed).unwrap();
        assert_ne!(identity, changed.embedding_key().unwrap(), "{field}");
    }
    let mut summary = summary_config("http://127.0.0.1:8080/v1/chat/completions".into());
    let identity = summary.summary_key().unwrap();
    summary.summary.as_mut().unwrap().extra_body = Some(
        json!({"chat_template_kwargs":{"enable_thinking":false}})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_ne!(identity, summary.summary_key().unwrap());
}

#[test]
fn non_loopback_endpoints_and_reserved_options_are_rejected() {
    for url in [
        "https://127.0.0.1/v1/embeddings",
        "http://example.com/v1/embeddings",
        "http://localhost.example.com/v1/embeddings",
        "http://localhost./v1/embeddings",
        "http://192.168.1.2/v1/embeddings",
        "http://0.0.0.0/v1/embeddings",
        "http://[::]/v1/embeddings",
        "http://[::ffff:192.168.1.2]/v1/embeddings",
        "http://secret@127.0.0.1/v1/embeddings",
        "http://127.0.0.1/v1/embeddings?secret=value",
        "http://127.0.0.1/v1/embeddings#fragment",
        "http://127.0.0.1:0/v1/embeddings",
        "file:///tmp/model",
    ] {
        assert_eq!(
            embedding_config(url.into()).validate().unwrap_err().code,
            "model_config_error",
            "{url}"
        );
    }
    for url in [
        "http://127.0.0.1:8080/v1/embeddings",
        "http://127.0.0.2:8080/v1/embeddings",
        "http://[::1]:8080/v1/embeddings",
        "http://localhost:8080/v1/embeddings",
    ] {
        assert!(embedding_config(url.into()).validate().is_ok(), "{url}");
    }
    for task_field in ["model", "input", "dimensions", "encoding_format", "", "x-y"] {
        let mut config = embedding_config("http://127.0.0.1:8080".into());
        config.embedding.as_mut().unwrap().task_field = Some(task_field.into());
        assert_eq!(config.validate().unwrap_err().code, "model_config_error");
    }
    for field in [
        "model",
        "messages",
        "temperature",
        "stream",
        "max_tokens",
        "tools",
        "n",
    ] {
        let mut config = summary_config("http://127.0.0.1:8080".into());
        config.summary.as_mut().unwrap().extra_body =
            Some(json!({field:"override"}).as_object().unwrap().clone());
        assert_eq!(config.validate().unwrap_err().code, "model_config_error");
    }
}

#[test]
fn embedding_request_preserves_query_and_document_purpose() {
    for purpose in ["query", "document"] {
        let (url, received, handle) = json_stub(embedding_response(json!([0.3, 0.0, 0.4])));
        let config = embedding_config(url.replace("127.0.0.1", "localhost"));
        assert_eq!(
            config.embed("Rust 记忆", purpose).unwrap(),
            vec![0.3, 0.0, 0.4]
        );
        let request = received.recv().unwrap();
        assert_eq!(request["model"], "test-embedding");
        assert_eq!(request["input"], "Rust 记忆");
        assert_eq!(request["input_type"], purpose);
        assert_eq!(request["dimensions"], 3);
        assert_eq!(request["encoding_format"], "float");
        handle.join().unwrap();
    }
}

#[test]
fn embedding_rejects_wrong_model_dimension_nonfinite_and_zero_vectors() {
    let good = embedding_response(json!([1, 2, 3]));
    let mut wrong_model = good.clone();
    wrong_model["model"] = json!("different-weights-alias");
    let mut wrong_index = good.clone();
    wrong_index["data"][0]["index"] = json!(1);
    let mut extra_output = good.clone();
    extra_output["data"] = json!([
        {"index":0,"embedding":[1,2,3]},
        {"index":1,"embedding":[1,2,3]}
    ]);
    for response in [
        embedding_response(json!([1, 2])),
        embedding_response(json!([0, 0, 0])),
        embedding_response(json!([1e100, 2, 3])),
        embedding_response(json!([1e-100, 0, 0])),
        embedding_response(json!(["1", 2, 3])),
        embedding_response(json!([null, 2, 3])),
        embedding_response(json!([])),
        wrong_model,
        wrong_index,
        extra_output,
        json!({"data":[{"index":0,"embedding":[1,2,3]}]}),
        json!({"model":"test-embedding","error":{"message":"private debug text"}}),
    ] {
        let (url, received, handle) = json_stub(response);
        let error = embedding_config(url)
            .embed("record", "document")
            .unwrap_err();
        assert_eq!(error.code, "model_invalid_response");
        assert!(!error.to_string().contains("private debug text"));
        received.recv().unwrap();
        handle.join().unwrap();
    }
}

#[test]
fn provider_http_errors_and_malformed_bodies_do_not_expose_response_content() {
    for (status, body, code, retryable) in [
        (
            "400 Bad Request",
            b"private server detail".to_vec(),
            "model_http_error",
            false,
        ),
        (
            "429 Too Many Requests",
            b"private server detail".to_vec(),
            "model_http_error",
            true,
        ),
        (
            "503 Unavailable",
            b"private server detail".to_vec(),
            "model_http_error",
            true,
        ),
        (
            "200 OK",
            b"private server detail".to_vec(),
            "model_invalid_response",
            false,
        ),
        ("200 OK", vec![0xff, 0xfe], "model_invalid_response", false),
    ] {
        let (url, received, handle) = stub(
            status,
            vec![("Content-Length".into(), body.len().to_string())],
            body,
            Duration::ZERO,
        );
        let error = embedding_config(url).embed("record", "query").unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.retryable, retryable);
        assert!(!error.to_string().contains("private server detail"));
        received.recv().unwrap();
        handle.join().unwrap();
    }
}

#[test]
fn redirects_are_never_followed_even_to_loopback() {
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let target_url = format!("http://{}/unexpected", target.local_addr().unwrap());
    let (url, received, handle) = stub(
        "307 Temporary Redirect",
        vec![
            ("Location".into(), target_url),
            ("Content-Length".into(), "0".into()),
        ],
        Vec::new(),
        Duration::ZERO,
    );
    assert_eq!(
        embedding_config(url)
            .embed("private input", "query")
            .unwrap_err()
            .code,
        "model_http_error"
    );
    received.recv().unwrap();
    handle.join().unwrap();
    assert_eq!(
        target.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn response_size_limit_applies_with_or_without_content_length() {
    for (headers, body) in [
        (
            vec![("Content-Length".into(), "2097153".into())],
            Vec::new(),
        ),
        (Vec::new(), vec![b' '; 2_097_153]),
    ] {
        let (url, received, handle) = stub("200 OK", headers, body, Duration::ZERO);
        assert_eq!(
            embedding_config(url)
                .embed("record", "query")
                .unwrap_err()
                .code,
            "model_response_too_large"
        );
        received.recv().unwrap();
        handle.join().unwrap();
    }
}

#[test]
fn summary_request_uses_untrusted_data_envelope_and_bounded_options() {
    let (url, received, handle) = json_stub(summary_response("Rust 是实现语言。"));
    let mut config = summary_config(url);
    config.summary.as_mut().unwrap().extra_body = Some(
        json!({"chat_template_kwargs":{"enable_thinking":false}})
            .as_object()
            .unwrap()
            .clone(),
    );
    let inputs = vec![(
        "source-id".into(),
        "The implementation language is Rust.".into(),
    )];
    assert_eq!(config.summarize(&inputs, 20).unwrap(), "Rust 是实现语言。");
    let request = received.recv().unwrap();
    assert_eq!(request["temperature"], 0);
    assert_eq!(request["stream"], false);
    assert_eq!(request["max_tokens"], 128);
    assert_eq!(request["chat_template_kwargs"]["enable_thinking"], false);
    assert_eq!(request["messages"][0]["role"], "system");
    assert!(request["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("untrusted data"));
    let envelope: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(envelope["type"], "untrusted_memory_records");
    assert_eq!(envelope["records"][0]["id"], "source-id");
    assert_eq!(envelope["records"][0]["text"], inputs[0].1);
    assert_eq!(envelope["max_chars"], 20);
    handle.join().unwrap();
}

#[test]
fn summary_rejects_empty_long_truncated_tool_or_wrong_model_results() {
    let mut truncated = summary_response("Partial sentence");
    truncated["choices"][0]["finish_reason"] = json!("length");
    let mut tool = summary_response("text");
    tool["choices"][0]["message"]["tool_calls"] = json!([{"name":"bad"}]);
    let mut refusal = summary_response("text");
    refusal["choices"][0]["message"]["refusal"] = json!("refused");
    let mut wrong_model = summary_response("text");
    wrong_model["model"] = json!("other-model");
    let mut missing_role = summary_response("text");
    missing_role["choices"][0]["message"]["role"] = Value::Null;
    for response in [
        summary_response(""),
        summary_response(" \n "),
        summary_response("too many characters for the requested budget"),
        truncated,
        tool,
        refusal,
        wrong_model,
        missing_role,
    ] {
        let (url, received, handle) = json_stub(response);
        assert_eq!(
            summary_config(url)
                .summarize(&[("id".into(), "source".into())], 20)
                .unwrap_err()
                .code,
            "model_invalid_response"
        );
        received.recv().unwrap();
        handle.join().unwrap();
    }
}

#[test]
fn input_bounds_fail_before_a_network_request() {
    let config = embedding_config("http://127.0.0.1:9/v1/embeddings".into());
    assert_eq!(
        config.embed("", "query").unwrap_err().code,
        "model_input_error"
    );
    assert_eq!(
        config.embed("text", "invalid").unwrap_err().code,
        "model_input_error"
    );
    assert_eq!(
        config
            .embed(&"a".repeat(65_537), "document")
            .unwrap_err()
            .code,
        "model_input_too_large"
    );
    assert_eq!(
        config
            .embed(&"\0".repeat(65_536), "document")
            .unwrap_err()
            .code,
        "model_input_too_large"
    );
    let config = summary_config("http://127.0.0.1:9/v1/chat/completions".into());
    assert_eq!(
        config.summarize(&[], 100).unwrap_err().code,
        "model_input_error"
    );
    assert_eq!(
        config
            .summarize(&[("id".into(), "a".repeat(65_536))], 100)
            .unwrap_err()
            .code,
        "model_input_too_large"
    );
    assert_eq!(
        config
            .summarize(&[("id".into(), "\0".repeat(60_000))], 100)
            .unwrap_err()
            .code,
        "model_input_too_large"
    );
    assert_eq!(
        config
            .summarize(&[("id".into(), "text".into())], 0)
            .unwrap_err()
            .code,
        "model_input_error"
    );
}

#[test]
fn request_timeout_is_reported_without_provider_details() {
    let body = serde_json::to_vec(&embedding_response(json!([1, 2, 3]))).unwrap();
    let (url, received, handle) = stub(
        "200 OK",
        vec![("Content-Length".into(), body.len().to_string())],
        body,
        Duration::from_millis(1300),
    );
    let mut config = embedding_config(url);
    config.embedding.as_mut().unwrap().timeout_seconds = 1;
    let error = config.embed("private input", "query").unwrap_err();
    assert_eq!(error.code, "model_timeout");
    assert!(error.retryable);
    assert!(!error.to_string().contains("private input"));
    received.recv().unwrap();
    handle.join().unwrap();
}

#[test]
fn response_timeout_is_enforced_after_success_headers() {
    let body = serde_json::to_vec(&embedding_response(json!([1, 2, 3]))).unwrap();
    let (url, received, handle) = response_stub(
        "200 OK",
        vec![("Content-Length".into(), body.len().to_string())],
        body,
        Duration::ZERO,
        Duration::from_millis(1300),
    );
    let mut config = embedding_config(url);
    config.embedding.as_mut().unwrap().timeout_seconds = 1;
    let error = config.embed("private input", "query").unwrap_err();
    assert_eq!(error.code, "model_timeout");
    assert!(error.retryable);
    received.recv().unwrap();
    handle.join().unwrap();
}
