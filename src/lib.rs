//! Local, revision-aware agent memory. Retrieved text is data, never authorization.
//!
//! All mutations and their idempotency receipts share a SQLite transaction.
//! Derived artifacts are published only after validating their captured read set.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const SCHEMA: i64 = 1;
const MAX_BODY: usize = 65_536;
const MAX_INPUTS: usize = 64;
const MAX_ANCESTORS: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl Error {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        let busy = matches!(
            &error,
            rusqlite::Error::SqliteFailure(e, _)
                if matches!(e.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        );
        Self {
            code: if busy { "busy" } else { "database_error" }.into(),
            message: if busy {
                "Database is busy; retry the same idempotency key."
            } else {
                "Database operation failed."
            }
            .into(),
            retryable: busy,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Store {
    connection: Connection,
}

#[derive(Debug, Clone, Serialize)]
struct Record {
    id: String,
    revision: i64,
    body: String,
    kind: String,
    source: String,
    volatility: String,
    status: String,
    valid_from: Option<i64>,
    valid_until: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

impl Store {
    /// Open a local database. A copied database retains its commit history identity.
    /// v0.1 has no backup restore/clone API; do not reuse old tokens across rollbacks.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path != Path::new(":memory:") {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                if !parent.exists() {
                    std::fs::create_dir_all(parent)
                        .map_err(|_| Error::new("io_error", "Cannot create database directory."))?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                            .map_err(|_| {
                                Error::new("io_error", "Cannot protect database directory.")
                            })?;
                    }
                }
            }
            if path
                .symlink_metadata()
                .is_ok_and(|m| !m.file_type().is_file())
            {
                return Err(Error::new(
                    "invalid_path",
                    "Database must be a regular file.",
                ));
            }
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(Error::new("io_error", "Cannot create database.")),
            }
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != 0 && version != SCHEMA {
            return Err(Error::new(
                "schema_mismatch",
                "Unsupported database schema version.",
            ));
        }
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if version == 0 {
            let unrelated: i64 = tx.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            // Another connection may have initialized while we waited for the write lock.
            let locked_version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if locked_version == 0 && unrelated > 0 {
                return Err(Error::new(
                    "schema_mismatch",
                    "Refusing to initialize a different database.",
                ));
            }
            if locked_version == 0 {
                tx.execute_batch(
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
                       PRIMARY KEY(child_id,parent_id),
                       FOREIGN KEY(child_id) REFERENCES records(id),
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
                     PRAGMA user_version=1;",
                )?;
                tx.execute(
                    "INSERT INTO meta(key,value) VALUES('store_id',?1),('sequence','0')",
                    [Uuid::new_v4().to_string()],
                )?;
            } else if locked_version != SCHEMA {
                return Err(Error::new(
                    "schema_mismatch",
                    "Unsupported database schema version.",
                ));
            }
        }
        tx.commit()?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Self { connection })
    }

    /// Execute a JSON command under a trusted namespace selected by the caller.
    /// Namespace separates data in the API; it is not authentication or OS isolation.
    pub fn execute(&mut self, namespace: &str, method: &str, arguments: Value) -> Result<Value> {
        validate_name(namespace, "namespace")?;
        let args = arguments
            .as_object()
            .ok_or_else(|| Error::new("invalid_request", "params must be an object."))?;
        let allowed: &[&str] = match method {
            "status" | "export" => &[],
            "remember" => &[
                "body",
                "kind",
                "source",
                "volatility",
                "valid_from",
                "valid_until",
                "idempotency_key",
            ],
            "inspect" => &["id", "history"],
            "revise" => &["id", "expected_revision", "body", "idempotency_key"],
            "retract" => &["id", "expected_revision", "idempotency_key"],
            "recall" => &["query", "limit", "max_chars", "minimum_commit"],
            "prepare" => &["inputs", "processor", "idempotency_key"],
            "job" => &["job_id"],
            "publish" => &["job_id", "body", "kind", "idempotency_key"],
            "operation" => &["method", "idempotency_key"],
            "plan" => &["strategy", "max_group_chars"],
            _ => return Err(Error::new("method_not_found", "Unknown method.")),
        };
        if args.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::new("invalid_request", "Unknown parameter."));
        }
        let mutation = matches!(
            method,
            "remember" | "revise" | "retract" | "prepare" | "publish"
        );
        let tx = self.connection.transaction_with_behavior(if mutation {
            TransactionBehavior::Immediate
        } else {
            TransactionBehavior::Deferred
        })?;
        let at = now();
        let result = if mutation {
            let key = string(&arguments, "idempotency_key")?;
            validate_name(key, "idempotency_key")?;
            let fingerprint = format!("{:x}", Sha256::digest(arguments.to_string().as_bytes()));
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT request_hash,receipt FROM operations WHERE namespace=?1 AND method=?2 AND key=?3",
                    params![namespace, method, key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((old_hash, receipt)) = prior {
                if old_hash != fingerprint {
                    return Err(Error::new(
                        "idempotency_conflict",
                        "Key already used with different arguments.",
                    ));
                }
                parse_stored(&receipt)?
            } else {
                let mut receipt = mutate(&tx, namespace, method, &arguments, at)?;
                tx.execute(
                    "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='sequence'",
                    [],
                )?;
                receipt["commit_token"] = json!(commit_token(&tx)?);
                receipt["durable"] = json!(true);
                tx.execute(
                    "INSERT INTO operations(namespace,method,key,request_hash,receipt) VALUES(?1,?2,?3,?4,?5)",
                    params![namespace, method, key, fingerprint, receipt.to_string()],
                )?;
                receipt
            }
        } else {
            read(&tx, namespace, method, &arguments, at)?
        };
        tx.commit()?;
        Ok(result)
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn parse_stored(s: &str) -> Result<Value> {
    serde_json::from_str(s).map_err(|_| Error::new("database_error", "Invalid stored metadata."))
}
fn validate_name(s: &str, label: &str) -> Result<()> {
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
        return Err(Error::new("invalid_request", format!("Invalid {label}.")));
    }
    Ok(())
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("invalid_request", format!("{key} must be a string.")))
}
fn optional_string<'a>(v: &'a Value, key: &str, default: &'a str) -> Result<&'a str> {
    match v.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_str()
            .ok_or_else(|| Error::new("invalid_request", format!("{key} must be a string."))),
    }
}
fn body(v: &Value) -> Result<&str> {
    let text = string(v, "body")?;
    if text.trim().is_empty() || text.len() > MAX_BODY {
        return Err(Error::new(
            "invalid_request",
            "body must be nonempty and at most 65536 UTF-8 bytes.",
        ));
    }
    Ok(text)
}
fn integer(v: &Value, key: &str, default: i64, min: i64, max: i64) -> Result<i64> {
    let n = match v.get(key) {
        None => default,
        Some(x) => x
            .as_i64()
            .ok_or_else(|| Error::new("invalid_request", format!("{key} must be an integer.")))?,
    };
    if n < min || n > max {
        return Err(Error::new(
            "invalid_request",
            format!("{key} is out of range."),
        ));
    }
    Ok(n)
}
fn optional_time(v: &Value, key: &str) -> Result<Option<i64>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => x.as_i64().filter(|x| *x >= 0).map(Some).ok_or_else(|| {
            Error::new(
                "invalid_request",
                format!("{key} must be a nonnegative Unix timestamp."),
            )
        }),
    }
}
fn kind(v: &Value, default: &str) -> Result<String> {
    let k = optional_string(v, "kind", default)?;
    if ![
        "assertion",
        "inference",
        "preference",
        "episode",
        "commitment",
        "summary",
    ]
    .contains(&k)
    {
        return Err(Error::new("invalid_request", "Unknown memory kind."));
    }
    Ok(k.into())
}
fn commit_token(db: &Connection) -> Result<String> {
    let id: String = db.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
        r.get(0)
    })?;
    let seq: String = db.query_row("SELECT value FROM meta WHERE key='sequence'", [], |r| {
        r.get(0)
    })?;
    Ok(format!("{id}:{seq}"))
}
fn check_commit(db: &Connection, v: &Value) -> Result<()> {
    if let Some(token) = v.get("minimum_commit") {
        let token = token
            .as_str()
            .ok_or_else(|| Error::new("invalid_request", "minimum_commit must be a string."))?;
        let current = commit_token(db)?;
        let (current_id, current_seq) = current.rsplit_once(':').unwrap();
        let (id, seq) = token
            .rsplit_once(':')
            .ok_or_else(|| Error::new("invalid_request", "Invalid commit token."))?;
        let seq: u64 = seq
            .parse()
            .map_err(|_| Error::new("invalid_request", "Invalid commit sequence."))?;
        if id != current_id {
            return Err(Error::new(
                "commit_history_mismatch",
                "Commit belongs to another storage history.",
            ));
        }
        if seq > current_seq.parse::<u64>().unwrap_or(0) {
            return Err(Error::new(
                "required_consistency_unavailable",
                "Requested commit is not available.",
            ));
        }
    }
    Ok(())
}
fn get(db: &Connection, ns: &str, id: &str) -> Result<Record> {
    db.query_row(
        "SELECT id,revision,body,kind,source,volatility,status,valid_from,valid_until,created_at,updated_at FROM records WHERE namespace=?1 AND id=?2",
        params![ns, id],
        |r| Ok(Record {
            id: r.get(0)?, revision: r.get(1)?, body: r.get(2)?, kind: r.get(3)?,
            source: r.get(4)?, volatility: r.get(5)?, status: r.get(6)?,
            valid_from: r.get(7)?, valid_until: r.get(8)?, created_at: r.get(9)?, updated_at: r.get(10)?,
        }),
    ).optional()?.ok_or_else(|| Error::new("not_found", "Record not found."))
}
fn temporally_valid(record: &Record, at: i64) -> bool {
    record.valid_from.is_none_or(|t| t <= at) && record.valid_until.is_none_or(|t| at < t)
}
fn dependencies(db: &Connection, id: &str) -> Result<Vec<(String, i64)>> {
    let mut stmt = db.prepare(
        "SELECT parent_id,parent_revision FROM edges WHERE child_id=?1 ORDER BY parent_id",
    )?;
    let rows = stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
fn valid(db: &Connection, ns: &str, record: &Record, at: i64) -> Result<bool> {
    if record.status != "active" || !temporally_valid(record, at) {
        return Ok(false);
    }
    let mut stack = dependencies(db, &record.id)?;
    let mut seen = HashSet::new();
    while let Some((id, revision)) = stack.pop() {
        if !seen.insert((id.clone(), revision)) {
            continue;
        }
        if seen.len() > MAX_ANCESTORS {
            return Err(Error::new(
                "resource_exhausted",
                "Dependency traversal limit exceeded.",
            ));
        }
        let parent = get(db, ns, &id)?;
        if parent.revision != revision
            || parent.status != "active"
            || !temporally_valid(&parent, at)
        {
            return Ok(false);
        }
        stack.extend(dependencies(db, &id)?);
    }
    Ok(true)
}
fn require_valid(db: &Connection, ns: &str, record: &Record, at: i64) -> Result<()> {
    if record.status == "retracted" {
        return Err(Error::new("not_found", "Record not found."));
    }
    if !valid(db, ns, record, at)? {
        return Err(Error::new(
            "stale_dependency",
            "Record or its dependencies are not current and valid.",
        ));
    }
    Ok(())
}
fn insert(db: &Connection, ns: &str, r: &Record) -> Result<()> {
    db.execute(
        "INSERT INTO records VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            r.id,
            ns,
            r.revision,
            r.body,
            r.kind,
            r.source,
            r.volatility,
            r.status,
            r.valid_from,
            r.valid_until,
            r.created_at,
            r.updated_at
        ],
    )?;
    db.execute(
        "INSERT INTO revisions VALUES(?1,?2,?3,?4,?5)",
        params![r.id, r.revision, ns, r.body, r.updated_at],
    )?;
    db.execute(
        "INSERT INTO record_fts(id,namespace,body) VALUES(?1,?2,?3)",
        params![r.id, ns, r.body],
    )?;
    Ok(())
}
fn descendants(db: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt = db.prepare(
        "WITH RECURSIVE affected(id) AS (
           SELECT child_id FROM edges WHERE parent_id=?1
           UNION SELECT edges.child_id FROM edges JOIN affected ON edges.parent_id=affected.id
         ) SELECT id FROM affected ORDER BY id",
    )?;
    let rows = stmt.query_map([id], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
fn invalidate(db: &Connection, ns: &str, ids: &[String], erase: bool, at: i64) -> Result<()> {
    for id in ids {
        db.execute(
            "UPDATE records SET status=?1,body=CASE WHEN ?2 THEN '' ELSE body END,updated_at=?3
             WHERE namespace=?4 AND id=?5 AND status!='retracted'",
            params![if erase { "retracted" } else { "stale" }, erase, at, ns, id],
        )?;
        db.execute(
            "DELETE FROM record_fts WHERE id=?1 AND namespace=?2",
            params![id, ns],
        )?;
        if erase {
            db.execute(
                "UPDATE revisions SET body='' WHERE id=?1 AND namespace=?2",
                params![id, ns],
            )?;
        }
        db.execute(
            "UPDATE jobs SET state='invalidated' WHERE namespace=?1 AND state='prepared'
             AND id IN (SELECT job_id FROM job_inputs WHERE record_id=?2)",
            params![ns, id],
        )?;
    }
    Ok(())
}
fn job_meta(db: &Connection, ns: &str, id: &str) -> Result<(String, String, Option<String>)> {
    db.query_row(
        "SELECT state,processor,output_id FROM jobs WHERE id=?1 AND namespace=?2",
        params![id, ns],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?
    .ok_or_else(|| Error::new("not_found", "Job not found."))
}
fn job_inputs(db: &Connection, ns: &str, id: &str, at: i64) -> Result<Vec<Record>> {
    let mut stmt =
        db.prepare("SELECT record_id,revision FROM job_inputs WHERE job_id=?1 ORDER BY record_id")?;
    let refs = stmt
        .query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if refs.is_empty() {
        return Err(Error::new("stale_dependency", "Job has no valid inputs."));
    }
    let mut records = Vec::new();
    for (id, revision) in refs {
        let r = get(db, ns, &id)?;
        if r.revision != revision || !valid(db, ns, &r, at)? {
            return Err(Error::new(
                "stale_dependency",
                "Captured input changed or is no longer valid.",
            ));
        }
        records.push(r);
    }
    Ok(records)
}

fn mutate(db: &Connection, ns: &str, method: &str, v: &Value, at: i64) -> Result<Value> {
    match method {
        "remember" => {
            let text = body(v)?;
            let source = optional_string(v, "source", "caller")?;
            validate_name(source, "source")?;
            let volatility = optional_string(v, "volatility", "normal")?;
            if !["stable", "normal", "volatile"].contains(&volatility) {
                return Err(Error::new("invalid_request", "Unknown volatility."));
            }
            let start = optional_time(v, "valid_from")?;
            let end = optional_time(v, "valid_until")?;
            if start.zip(end).is_some_and(|(a, b)| a >= b) {
                return Err(Error::new(
                    "invalid_request",
                    "valid_until must be after valid_from.",
                ));
            }
            let r = Record {
                id: Uuid::new_v4().to_string(),
                revision: 1,
                body: text.into(),
                kind: kind(v, "assertion")?,
                source: source.into(),
                volatility: volatility.into(),
                status: "active".into(),
                valid_from: start,
                valid_until: end,
                created_at: at,
                updated_at: at,
            };
            insert(db, ns, &r)?;
            Ok(json!({"id":r.id,"revision":1,"state":"active","index_state":"lexical_ready"}))
        }
        "revise" | "retract" => {
            let id = string(v, "id")?;
            let r = get(db, ns, id)?;
            if r.status == "retracted" {
                return Err(Error::new("not_found", "Record not found."));
            }
            let expected = integer(v, "expected_revision", -1, 1, i64::MAX)?;
            if expected != r.revision {
                return Err(Error::new("revision_conflict", "Record revision changed."));
            }
            let affected = descendants(db, id)?;
            if method == "retract" {
                let mut all = affected.clone();
                all.push(id.into());
                invalidate(db, ns, &all, true, at)?;
                return Ok(json!({"id":id,"revision":r.revision,"state":"retracted",
                    "withdrawn_descendants":affected.len(),"physical_erasure":false}));
            }
            require_valid(db, ns, &r, at)?;
            let text = body(v)?;
            let revision = r
                .revision
                .checked_add(1)
                .ok_or_else(|| Error::new("resource_exhausted", "Revision limit reached."))?;
            invalidate(db, ns, &affected, false, at)?;
            db.execute(
                "UPDATE jobs SET state='invalidated' WHERE namespace=?1 AND state='prepared'
                 AND id IN (SELECT job_id FROM job_inputs WHERE record_id=?2)",
                params![ns, id],
            )?;
            db.execute(
                "UPDATE records SET revision=?1,body=?2,updated_at=?3 WHERE namespace=?4 AND id=?5",
                params![revision, text, at, ns, id],
            )?;
            db.execute(
                "INSERT INTO revisions VALUES(?1,?2,?3,?4,?5)",
                params![id, revision, ns, text, at],
            )?;
            db.execute(
                "DELETE FROM record_fts WHERE id=?1 AND namespace=?2",
                params![id, ns],
            )?;
            db.execute(
                "INSERT INTO record_fts(id,namespace,body) VALUES(?1,?2,?3)",
                params![id, ns, text],
            )?;
            Ok(
                json!({"id":id,"revision":revision,"state":"active","invalidated_descendants":affected.len(),
                "index_state":"lexical_ready"}),
            )
        }
        "prepare" => {
            let inputs = v.get("inputs").and_then(Value::as_array).ok_or_else(|| {
                Error::new("invalid_request", "inputs must be an array of record IDs.")
            })?;
            if inputs.is_empty() || inputs.len() > MAX_INPUTS {
                return Err(Error::new(
                    "invalid_request",
                    "Prepare requires 1 to 64 inputs.",
                ));
            }
            let processor = string(v, "processor")?;
            validate_name(processor, "processor")?;
            let mut seen = HashSet::new();
            let mut records = Vec::new();
            for input in inputs {
                let id = input
                    .as_str()
                    .ok_or_else(|| Error::new("invalid_request", "Input must be a record ID."))?;
                if !seen.insert(id) {
                    return Err(Error::new("invalid_request", "Duplicate input ID."));
                }
                let r = get(db, ns, id)?;
                require_valid(db, ns, &r, at)?;
                records.push(r);
            }
            let id = Uuid::new_v4().to_string();
            db.execute(
                "INSERT INTO jobs VALUES(?1,?2,?3,'prepared',NULL,?4)",
                params![id, ns, processor, at],
            )?;
            let mut deps = Vec::new();
            for r in records {
                db.execute(
                    "INSERT INTO job_inputs VALUES(?1,?2,?3)",
                    params![id, r.id, r.revision],
                )?;
                deps.push(json!({"id":r.id,"revision":r.revision}));
            }
            Ok(json!({"job_id":id,"state":"prepared","dependencies":deps}))
        }
        "publish" => {
            let id = string(v, "job_id")?;
            let text = body(v)?;
            let (state, processor, _) = job_meta(db, ns, id)?;
            if state == "invalidated" {
                return Err(Error::new(
                    "stale_dependency",
                    "Captured inputs were invalidated.",
                ));
            }
            if state != "prepared" {
                return Err(Error::new(
                    "job_state_conflict",
                    "Job has already published.",
                ));
            }
            let inputs = job_inputs(db, ns, id, at)?;
            let r = Record {
                id: Uuid::new_v4().to_string(),
                revision: 1,
                body: text.into(),
                kind: kind(v, "summary")?,
                source: processor,
                volatility: "normal".into(),
                status: "active".into(),
                valid_from: None,
                valid_until: None,
                created_at: at,
                updated_at: at,
            };
            insert(db, ns, &r)?;
            for input in inputs {
                db.execute(
                    "INSERT INTO edges VALUES(?1,?2,?3)",
                    params![r.id, input.id, input.revision],
                )?;
            }
            db.execute(
                "UPDATE jobs SET state='published',output_id=?1 WHERE id=?2 AND namespace=?3",
                params![r.id, id, ns],
            )?;
            Ok(
                json!({"id":r.id,"revision":1,"job_id":id,"state":"published","index_state":"lexical_ready"}),
            )
        }
        _ => Err(Error::new("method_not_found", "Unknown mutation.")),
    }
}

fn readable(db: &Connection, ns: &str, at: i64) -> Result<Vec<Record>> {
    let mut stmt = db.prepare(
        "SELECT id FROM records WHERE namespace=?1 AND status='active' ORDER BY updated_at DESC,id",
    )?;
    let ids = stmt
        .query_map([ns], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut records = Vec::new();
    for id in ids {
        let r = get(db, ns, &id)?;
        if valid(db, ns, &r, at)? {
            records.push(r);
        }
    }
    Ok(records)
}
fn read(db: &Connection, ns: &str, method: &str, v: &Value, at: i64) -> Result<Value> {
    match method {
        "status" => {
            let count: i64 = db.query_row(
                "SELECT count(*) FROM records WHERE namespace=?1",
                [ns],
                |r| r.get(0),
            )?;
            Ok(
                json!({"version":VERSION,"schema_version":SCHEMA,"namespace":ns,"commit_token":commit_token(db)?,
                "records":count,"capabilities":{"lexical_search":true,"embedding":false,"model_calls":false,
                "lineage":true,"conditional_publish":true,"restore":false,"physical_erasure":false}}),
            )
        }
        "inspect" => {
            let id = string(v, "id")?;
            let r = get(db, ns, id)?;
            if r.status == "retracted" {
                return Err(Error::new("not_found", "Record not found."));
            }
            let history = match v.get("history") {
                None => false,
                Some(x) => x
                    .as_bool()
                    .ok_or_else(|| Error::new("invalid_request", "history must be a boolean."))?,
            };
            if !history {
                require_valid(db, ns, &r, at)?;
            }
            let mut result = json!(r);
            result["applicable_now"] = json!(valid(db, ns, &r, at)?);
            result["trust_boundary"] = json!("retrieved_data");
            result["dependencies"] = json!(dependencies(db, id)?
                .into_iter()
                .map(|(id, revision)| json!({"id":id,"revision":revision}))
                .collect::<Vec<_>>());
            result["commit_token"] = json!(commit_token(db)?);
            if history {
                let mut stmt = db.prepare("SELECT revision,body,recorded_at FROM revisions WHERE namespace=?1 AND id=?2 ORDER BY revision")?;
                let rows = stmt.query_map(params![ns, id], |row| Ok(json!({
                    "revision":row.get::<_,i64>(0)?,"body":row.get::<_,String>(1)?,"recorded_at":row.get::<_,i64>(2)?,"historical":true
                })))?;
                result["history"] = json!(rows.collect::<std::result::Result<Vec<_>, _>>()?);
            }
            Ok(result)
        }
        "recall" => recall(db, ns, v, at),
        "job" => {
            let id = string(v, "job_id")?;
            let (state, processor, output) = job_meta(db, ns, id)?;
            if state == "invalidated" {
                return Err(Error::new(
                    "stale_dependency",
                    "Job inputs were invalidated.",
                ));
            }
            // Published jobs are also revalidated; historical receipts never grant body access.
            let inputs = job_inputs(db, ns, id, at)?;
            Ok(
                json!({"job_id":id,"state":state,"processor":processor,"output_id":output,
                "inputs":inputs,"trust_boundary":"retrieved_data","commit_token":commit_token(db)?}),
            )
        }
        "operation" => {
            let method = string(v, "method")?;
            let key = string(v, "idempotency_key")?;
            let result: Option<String> = db
                .query_row(
                    "SELECT receipt FROM operations WHERE namespace=?1 AND method=?2 AND key=?3",
                    params![ns, method, key],
                    |r| r.get(0),
                )
                .optional()?;
            parse_stored(&result.ok_or_else(|| {
                Error::new(
                    "not_found",
                    "No committed receipt found; retry the original key.",
                )
            })?)
        }
        "export" => {
            let records = readable(db, ns, at)?;
            let included: HashSet<_> = records.iter().map(|r| r.id.as_str()).collect();
            let mut edges = Vec::new();
            for r in &records {
                for (id, revision) in dependencies(db, &r.id)? {
                    if included.contains(id.as_str()) {
                        edges.push(
                            json!({"child_id":r.id,"parent_id":id,"parent_revision":revision}),
                        );
                    }
                }
            }
            Ok(
                json!({"schema_version":"recall-ledger.snapshot/1","namespace":ns,"records":records,
                "edges":edges,"commit_token":commit_token(db)?,"restore_supported":false}),
            )
        }
        "plan" => plan(db, ns, v, at),
        _ => Err(Error::new("method_not_found", "Unknown read.")),
    }
}

fn recall(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    check_commit(db, v)?;
    let query = optional_string(v, "query", "")?;
    if query.len() > 4096 {
        return Err(Error::new("invalid_request", "query is too large."));
    }
    let limit = integer(v, "limit", 10, 1, 100)? as usize;
    let budget = integer(v, "max_chars", 4000, 0, 1_000_000)? as usize;
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(32)
        .map(|t| t.to_lowercase())
        .collect();
    let mut ranks: BTreeMap<String, f64> = BTreeMap::new();
    if !terms.is_empty() {
        let expr = terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut stmt = db.prepare(
            "SELECT id FROM record_fts WHERE record_fts MATCH ?1 AND namespace=?2 ORDER BY bm25(record_fts),id LIMIT 1000",
        )?;
        let ids = stmt.query_map(params![expr, ns], |r| r.get::<_, String>(0))?;
        for (rank, id) in ids.enumerate() {
            ranks.insert(id?, 1.0 / (60.0 + rank as f64 + 1.0));
        }
    }
    let records = readable(db, ns, at)?;
    let mut candidates: Vec<(f64, Record)> = Vec::new();
    for r in records {
        let lower = r.body.to_lowercase();
        let matched = terms.iter().filter(|t| lower.contains(t.as_str())).count();
        if terms.is_empty() {
            if query.trim().is_empty() || lower.contains(&query.to_lowercase()) {
                candidates.push((0.0, r));
            }
        } else if matched > 0 || ranks.contains_key(&r.id) {
            // Literal substring channel keeps CJK useful without loading an embedding model.
            let score = ranks.get(&r.id).copied().unwrap_or(0.0)
                + matched as f64 / (60.0 + terms.len() as f64);
            candidates.push((score, r));
        }
    }
    candidates.sort_by(|(a, ar), (b, br)| {
        b.total_cmp(a)
            .then_with(|| br.updated_at.cmp(&ar.updated_at))
            .then_with(|| ar.id.cmp(&br.id))
    });
    let mut items = Vec::new();
    let mut used = 0usize;
    let mut deferred = 0;
    for (score, r) in candidates {
        let chars = r.body.chars().count();
        if items.len() == limit || used + chars > budget {
            deferred += 1;
            continue;
        }
        used += chars;
        let mut item = json!(r);
        item["selection_score"] = json!(score);
        item["trust_boundary"] = json!("retrieved_data");
        items.push(item);
    }
    Ok(json!({"items":items,"commit_token":commit_token(db)?,
        "budget":{"max_chars":budget,"used_chars":used,"unit":"unicode_scalars","bodies_only":true},
        "coverage":{"status":if deferred>0 {"budget_limited"} else {"complete_scan"},
        "deferred":deferred,"semantic_search":false,"scope":"active_current_records"}}))
}

fn plan(db: &Connection, ns: &str, v: &Value, at: i64) -> Result<Value> {
    let strategy = optional_string(v, "strategy", "topic")?;
    if !["individual", "fixed", "topic", "volatility"].contains(&strategy) {
        return Err(Error::new("invalid_request", "Unknown grouping strategy."));
    }
    let max_chars = integer(v, "max_group_chars", 4000, 1, 1_000_000)? as usize;
    let mut buckets: BTreeMap<String, Vec<Record>> = BTreeMap::new();
    for r in readable(db, ns, at)? {
        // Scope is fixed by the caller. Source/kind are conservative hard grouping boundaries.
        let suffix = match strategy {
            "individual" => r.id.clone(),
            "volatility" => r.volatility.clone(),
            "topic" => r
                .body
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_lowercase(),
            _ => String::new(),
        };
        let key = serde_json::to_string(&(&r.source, &r.kind, suffix)).unwrap();
        buckets.entry(key).or_default().push(r);
    }
    let mut groups = Vec::new();
    let mut oversized = Vec::new();
    for (key, records) in buckets {
        let mut ids = Vec::<String>::new();
        let mut chars = 0;
        for r in records {
            let n = r.body.chars().count();
            if n > max_chars {
                oversized.push(r.id);
                continue;
            }
            if !ids.is_empty() && (chars + n > max_chars || ids.len() == MAX_INPUTS) {
                groups.push(json!({"key":key,"inputs":ids,"chars":chars}));
                ids = Vec::new();
                chars = 0;
            }
            ids.push(r.id);
            chars += n;
        }
        if !ids.is_empty() {
            groups.push(json!({"key":key,"inputs":ids,"chars":chars}));
        }
    }
    Ok(
        json!({"strategy":strategy,"groups":groups,"oversized":oversized,"max_group_chars":max_chars,
        "experimental":true,"topic_method":"first_whitespace_token","commit_token":commit_token(db)?,
        "note":"Deterministic input grouping; not semantic summarization or a validated quality optimization."}),
    )
}
