//! Bounded local IPC. Namespace is selected by the daemon operator, never a request.

use recall_ledger::Error;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::Path;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_ID_BYTES: usize = 256;

pub fn failure(code: &str, message: impl Into<String>, retryable: bool) -> Error {
    Error {
        code: code.into(),
        message: message.into(),
        retryable,
    }
}

/// A newline is mandatory, including on the last message. Allocation is bounded.
pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unterminated JSON frame",
                ))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if frame.len().saturating_add(take) > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSON frame exceeds 1 MiB",
            ));
        }
        frame.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            frame.pop();
            return Ok(Some(frame));
        }
    }
}

pub fn write_frame(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "JSON response exceeds 1 MiB",
        ));
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub fn valid_id(id: &Value) -> bool {
    // IDs are reflected in replies. Bound them before executing a mutation so a
    // valid input frame cannot consume the entire response envelope budget.
    id.as_str().is_some_and(|value| value.len() <= MAX_ID_BYTES) || id.is_i64() || id.is_u64()
}

#[cfg(unix)]
pub fn request(socket: &Path, method: &str, params: Value) -> Result<Value, Error> {
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = UnixStream::connect(socket)
        .map_err(|err| failure("transport_unavailable", err.to_string(), true))?;
    let timeout = Some(Duration::from_secs(15));
    stream
        .set_read_timeout(timeout)
        .and_then(|()| stream.set_write_timeout(timeout))
        .map_err(|err| failure("transport_error", err.to_string(), true))?;
    let id = uuid::Uuid::new_v4().to_string();
    write_frame(
        &mut stream,
        &json!({"id": id, "method": method, "params": params}),
    )
    .map_err(|err| failure("transport_error", err.to_string(), true))?;
    let frame = read_frame(&mut BufReader::new(stream))
        .map_err(|err| failure("transport_error", err.to_string(), true))?
        .ok_or_else(|| failure("transport_error", "daemon closed without a response", true))?;
    let response: Value = serde_json::from_slice(&frame)
        .map_err(|err| failure("transport_error", err.to_string(), true))?;
    if response.get("id") != Some(&Value::String(id)) {
        return Err(failure("transport_error", "response ID mismatch", false));
    }
    match (response.get("result"), response.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(failure(
            error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("backend_error"),
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("backend error"),
            error
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )),
        _ => Err(failure(
            "transport_error",
            "invalid response envelope",
            false,
        )),
    }
}

#[cfg(not(unix))]
pub fn request(_socket: &Path, _method: &str, _params: Value) -> Result<Value, Error> {
    Err(failure(
        "unsupported_platform",
        "IPC requires macOS or Linux",
        false,
    ))
}

#[cfg(unix)]
mod daemon {
    use super::*;
    use fs2::FileExt;
    use recall_ledger::Store;
    use std::fs::{self, File, OpenOptions};
    use std::io::BufReader;
    use std::os::unix::fs::{
        DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt,
    };
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    const MAX_CLIENTS: usize = 32;

    struct PrivateSocket {
        path: PathBuf,
        dev: u64,
        ino: u64,
        _lock: File,
    }

    impl Drop for PrivateSocket {
        fn drop(&mut self) {
            if let Ok(metadata) = fs::symlink_metadata(&self.path) {
                if metadata.dev() == self.dev && metadata.ino() == self.ino {
                    let _ = fs::remove_file(&self.path);
                }
            }
        }
    }

    struct ClientSlot(Arc<AtomicUsize>);

    impl Drop for ClientSlot {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn private_parent(path: &Path) -> io::Result<PathBuf> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !parent.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        fs::canonicalize(parent)
    }

    fn locked_file(path: &Path) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        file.try_lock_exclusive()?;
        Ok(file)
    }

    fn listener(socket: &Path) -> io::Result<(UnixListener, PrivateSocket)> {
        let parent = private_parent(socket)?;
        let filename = socket.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "socket needs a filename")
        })?;
        let path = parent.join(filename);
        let mut lock_name = filename.to_os_string();
        lock_name.push(".lock");
        let lock = locked_file(&parent.join(lock_name))?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "socket path is not a socket; refusing to remove it",
                    ));
                }
                match UnixStream::connect(&path) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "socket is already serving",
                        ))
                    }
                    Err(err)
                        if err.kind() == io::ErrorKind::ConnectionRefused
                            || err.kind() == io::ErrorKind::NotFound =>
                    {
                        fs::remove_file(&path)?;
                    }
                    Err(err) => return Err(err),
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        // A restrictive staging directory prevents connections before chmod.
        let staging = parent.join(format!(
            ".rl-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        fs::DirBuilder::new().mode(0o700).create(&staging)?;
        let temporary = staging.join("s");
        let bound = (|| {
            let listener = UnixListener::bind(&temporary)?;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
            fs::rename(&temporary, &path)?;
            Ok::<_, io::Error>(listener)
        })();
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_dir(&staging);
        let listener = bound?;
        let metadata = fs::symlink_metadata(&path)?;
        Ok((
            listener,
            PrivateSocket {
                path,
                dev: metadata.dev(),
                ino: metadata.ino(),
                _lock: lock,
            },
        ))
    }

    fn response(store: &mut Store, namespace: &str, frame: &[u8]) -> Value {
        let request: Value = match serde_json::from_slice(frame) {
            Ok(value) => value,
            Err(_) => {
                return json!({"id": null, "error": failure("invalid_json", "expected a JSON object", false)})
            }
        };
        let id = request
            .get("id")
            .filter(|id| valid_id(id))
            .cloned()
            .unwrap_or(Value::Null);
        let object = match request.as_object() {
            Some(object) if !id.is_null() => object,
            _ => {
                return json!({"id": id, "error": failure("invalid_request", "request requires an integer id or string id of at most 256 UTF-8 bytes", false)})
            }
        };
        if object
            .keys()
            .any(|key| !["id", "method", "params"].contains(&key.as_str()))
        {
            return json!({"id": id, "error": failure("invalid_request", "unknown envelope field; namespace is pinned by daemon", false)});
        }
        let method = match request.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => {
                return json!({"id": id, "error": failure("invalid_request", "method must be a string", false)})
            }
        };
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() || params.get("namespace").is_some() {
            return json!({"id": id, "error": failure("invalid_request", "params must be an object without namespace", false)});
        }
        match store.execute(namespace, method, params) {
            Ok(result) => json!({"id": id, "result": result}),
            Err(error) => json!({"id": id, "error": error}),
        }
    }

    fn connection(stream: UnixStream, db: &Path, namespace: &str) -> io::Result<()> {
        let timeout = Some(Duration::from_secs(15));
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        let mut store = Store::open(db).map_err(io::Error::other)?;
        loop {
            let frame = match read_frame(&mut reader) {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(()),
                Err(err) => {
                    let code = if err.kind() == io::ErrorKind::InvalidData {
                        "message_too_large"
                    } else {
                        "transport_error"
                    };
                    let _ = write_frame(
                        &mut writer,
                        &json!({"id": null, "error": failure(code, err.to_string(), false)}),
                    );
                    return Err(err);
                }
            };
            let result = response(&mut store, namespace, &frame);
            if let Err(err) = write_frame(&mut writer, &result) {
                if err.kind() != io::ErrorKind::InvalidData {
                    return Err(err);
                }
                write_frame(
                    &mut writer,
                    &json!({"id": result["id"], "error": failure("response_too_large", "result exceeds IPC limit; use smaller reads or trusted CLI export", false)}),
                )?;
            }
        }
    }

    pub fn serve(db: &Path, namespace: &str, socket: &Path) -> Result<(), Error> {
        if namespace.trim().is_empty() {
            return Err(failure(
                "invalid_namespace",
                "namespace cannot be empty",
                false,
            ));
        }
        if db == Path::new(":memory:")
            || fs::symlink_metadata(db).is_ok_and(|metadata| !metadata.file_type().is_file())
        {
            return Err(failure(
                "invalid_path",
                "daemon requires a regular on-disk database file",
                false,
            ));
        }
        let parent =
            private_parent(db).map_err(|err| failure("io_error", err.to_string(), false))?;
        let filename = db
            .file_name()
            .ok_or_else(|| failure("invalid_path", "database needs a filename", false))?;
        let db = parent.join(filename);
        if fs::metadata(&db).is_ok_and(|metadata| metadata.nlink() > 1) {
            return Err(failure(
                "invalid_path",
                "hard-linked SQLite databases are not supported by the daemon",
                false,
            ));
        }
        let mut lock_name = filename.to_os_string();
        lock_name.push(".daemon.lock");
        // Never flock SQLite itself: flock and SQLite's fcntl locks interact on
        // macOS. A sidecar on the canonical database path owns daemon lifetime.
        let _database_lock = locked_file(&parent.join(lock_name)).map_err(|err| {
            failure(
                "daemon_lock",
                format!("cannot acquire database daemon lock: {err}"),
                false,
            )
        })?;
        // Fail startup, rather than accepting clients against an invalid database.
        let mut store = Store::open(&db)?;
        store.execute(namespace, "status", json!({}))?;
        drop(store);
        let (listener, _socket_guard) =
            listener(socket).map_err(|err| failure("socket_error", err.to_string(), false))?;
        eprintln!(
            "recall-ledger listening on {} (namespace {})",
            socket.display(),
            namespace
        );
        let active = Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming() {
            let stream = stream.map_err(|err| failure("transport_error", err.to_string(), true))?;
            if active.fetch_add(1, Ordering::SeqCst) >= MAX_CLIENTS {
                active.fetch_sub(1, Ordering::SeqCst);
                drop(stream);
                continue;
            }
            let slot = ClientSlot(Arc::clone(&active));
            let db = db.clone();
            let namespace = namespace.to_owned();
            if let Err(err) = std::thread::Builder::new()
                .name("recall-client".into())
                .spawn(move || {
                    let _slot = slot;
                    // A malformed/disconnected client must not terminate the daemon.
                    let _ = connection(stream, &db, &namespace);
                })
            {
                eprintln!("cannot spawn client handler: {err}");
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
pub use daemon::serve;

#[cfg(not(unix))]
pub fn serve(_db: &Path, _namespace: &str, _socket: &Path) -> Result<(), Error> {
    Err(failure(
        "unsupported_platform",
        "IPC requires macOS or Linux",
        false,
    ))
}
