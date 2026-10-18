//! Offline model-cost reservations for a trusted Benchmark controller.
//!
//! This is a single local SQLite ledger, including multiple processes on that
//! host. It is not a distributed gateway, a provider billing limit, or an EBS
//! multi-writer protocol. Never give an untrusted Agent access to this database.
//! Monetary JSON fields are decimal integer strings, in millionths of USD.

use clap::{Args, Parser, Subcommand};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::fs;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const APPLICATION_ID: i64 = 0x524c4244;
const SCHEMA: i64 = 1;
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Parser)]
#[command(
    name = "recall-bench-budget",
    version,
    about = "Offline, persistent model-cost budget for a trusted local Benchmark controller",
    after_help = "USD amounts use decimal notation with at most 6 fractional digits. \
All monetary JSON fields are decimal integer strings in micro-USD. \
The caller must reserve a bound covering every billable component, including \
input, output, tools, embedding and judging where applicable. \
This executable does not call providers or cap their invoices. \
Use a dedicated owner-only local directory; do not share live SQLite across nodes."
)]
struct Cli {
    #[arg(long)]
    db: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize an immutable budget identity and limit; same arguments are idempotent.
    Init {
        #[arg(long)]
        budget_id: String,
        #[arg(long, allow_hyphen_values = true)]
        limit_usd: String,
    },
    /// Reserve the full possible charge before any external request is dispatched.
    Reserve(Reserve),
    /// Claim permission to send exactly once. A repeated claim never grants permission again.
    MarkDispatched {
        #[arg(long)]
        request_id: String,
    },
    /// Record one final charge, retaining real overruns and halting further dispatch.
    Settle {
        #[arg(long)]
        request_id: String,
        #[arg(long, allow_hyphen_values = true)]
        actual_usd: String,
        #[arg(long)]
        cost_event_id: String,
        #[arg(long)]
        provider_request_id: String,
    },
    /// Release only a reservation that has never been marked dispatched.
    Cancel {
        #[arg(long)]
        request_id: String,
    },
    /// Stop new reservations and dispatch. Outstanding calls remain held and can settle.
    Close,
    /// Reopen a closed ledger without resetting costs, IDs, reservations, or an overrun halt.
    Reopen,
    /// Read a consistent accounting snapshot, optionally including one request.
    Snapshot {
        #[arg(long)]
        request_id: Option<String>,
    },
}

#[derive(Args, Clone)]
struct Reserve {
    #[arg(long)]
    request_id: String,
    /// SHA-256 of the exact provider request, including its model and limits.
    #[arg(long)]
    request_sha256: String,
    #[arg(long)]
    run_id: String,
    /// SHA-256 identity from the frozen configuration lock.
    #[arg(long)]
    configuration_id: String,
    /// Provider plus billing account/project; provider request IDs are unique within this scope.
    #[arg(long)]
    provider_scope: String,
    #[arg(long)]
    operation: String,
    #[arg(long)]
    price_snapshot_id: String,
    #[arg(long, value_parser = ["D0", "P0", "P1", "P2", "P3", "P4", "P5"])]
    stage: String,
    #[arg(long, value_parser = ["system_runtime", "evaluation", "development", "infrastructure"])]
    ledger: String,
    /// Conservative upper bound for this entire billable request.
    #[arg(long, allow_hyphen_values = true)]
    bound_usd: String,
}

#[derive(Debug)]
struct Error {
    code: &'static str,
    message: String,
}

type Result<T> = std::result::Result<T, Error>;

fn error(code: &'static str, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}

impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        error("database_error", value.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        error("io_error", value.to_string())
    }
}

/// No floats, exponent notation, signs, rounding, or silently truncated precision.
fn money(value: &str, positive: bool) -> Result<i64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 6
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (value.contains('.') && fraction.is_empty())
    {
        return Err(error(
            "invalid_amount",
            "Use unsigned decimal USD with at most 6 fractional digits.",
        ));
    }
    let whole = whole
        .parse::<i64>()
        .map_err(|_| error("invalid_amount", "USD amount exceeds the integer range."))?;
    let fraction_value = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i64>()
            .map_err(|_| error("invalid_amount", "Invalid fractional USD."))?
            * 10_i64.pow((6 - fraction.len()) as u32)
    };
    let amount = whole
        .checked_mul(1_000_000)
        .and_then(|n| n.checked_add(fraction_value))
        .ok_or_else(|| error("invalid_amount", "USD amount exceeds the integer range."))?;
    if positive && amount == 0 {
        return Err(error("invalid_amount", "This amount must be positive."));
    }
    Ok(amount)
}

fn identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_whitespace) {
        return Err(error(
            "invalid_identifier",
            "Identifiers must have 1–256 bytes and contain no whitespace.",
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(error(
            "invalid_identifier",
            "Identifiers cannot contain controls.",
        ));
    }
    Ok(())
}

fn sha256(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(error(
            "invalid_digest",
            "Configuration and request hashes must be lowercase SHA-256 hex.",
        ));
    }
    Ok(())
}

fn now_ms() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .ok_or_else(|| error("clock_error", "Cannot represent the current timestamp."))
}

#[cfg(unix)]
fn private_file(path: &Path, required: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    match fs::symlink_metadata(path) {
        Ok(meta)
            if meta.is_file()
                && !meta.file_type().is_symlink()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o777 == 0o600
                && meta.nlink() == 1 =>
        {
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => Ok(()),
        Ok(meta) => Err(error(
            "invalid_path",
            format!(
                "Database and sidecars must be owner-owned regular files with mode 0600 and one link \
                 (regular={}, symlink={}, owner={}, mode={:04o}, links={}).",
                meta.is_file(),
                meta.file_type().is_symlink(),
                meta.uid() == unsafe { libc::geteuid() },
                meta.mode() & 0o777,
                meta.nlink()
            ),
        )),
        Err(e) => Err(error(
            "invalid_path",
            format!("Cannot validate database or sidecar metadata: {e}"),
        )),
    }
}

#[cfg(unix)]
fn prepare_path(path: &Path, create: bool) -> Result<(PathBuf, fs::File)> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    if path.as_os_str().is_empty() || path == Path::new(":memory:") || path.file_name().is_none() {
        return Err(error(
            "invalid_path",
            "A persistent local database path is required.",
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if create && !parent.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let meta = fs::symlink_metadata(parent)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err(error(
            "invalid_path",
            "Use a dedicated owner-owned directory with mode 0700.",
        ));
    }
    let absolute = parent.canonicalize()?.join(path.file_name().unwrap());
    // Serialize the complete connection lifetime, including SQLite's last-close
    // sidecar cleanup. The separate inode must never be unlinked or be the DB.
    let operation_lock = acquire_operation_lock(&absolute, LOCK_TIMEOUT)?;
    if create {
        match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&absolute)
        {
            Ok(file) => file.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    private_file(&absolute, true)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = absolute.as_os_str().to_os_string();
        sidecar.push(suffix);
        private_file(Path::new(&sidecar), false)?;
    }
    Ok((absolute, operation_lock))
}

#[cfg(not(unix))]
fn prepare_path(_path: &Path, _create: bool) -> Result<(PathBuf, fs::File)> {
    Err(error(
        "unsupported_platform",
        "This version requires Unix owner-only filesystem permissions.",
    ))
}

#[cfg(unix)]
fn acquire_operation_lock(path: &Path, timeout: Duration) -> Result<fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".operation.lock");
    let lock_path = Path::new(&lock_path);
    private_file(lock_path, false)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(lock_path)
        .map_err(|e| error("invalid_path", format!("Cannot open operation lock: {e}")))?;
    let validate = || -> Result<()> {
        private_file(lock_path, true)?;
        let opened = file.metadata()?;
        let named = fs::symlink_metadata(lock_path)?;
        if !opened.is_file()
            || opened.uid() != unsafe { libc::geteuid() }
            || opened.mode() & 0o777 != 0o600
            || opened.nlink() != 1
            || opened.dev() != named.dev()
            || opened.ino() != named.ino()
        {
            return Err(error(
                "invalid_path",
                "Operation lock must remain the same owner-only regular file with one link.",
            ));
        }
        Ok(())
    };
    validate()?;
    let deadline = Instant::now() + timeout;
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(error(
                        "operation_lock_timeout",
                        "Timed out waiting for another local budget operation.",
                    ));
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(e) => return Err(e.into()),
        }
    }
    // A changed/unlinked lock inode must not admit a second independent owner.
    validate()?;
    Ok(file)
}

struct Database {
    // Rust drops fields in declaration order: close SQLite (and its sidecars)
    // before releasing the operation lock. Keep this ordering.
    connection: Connection,
    _operation_lock: fs::File,
}

impl Deref for Database {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for Database {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

fn enable_wal(db: &Connection, timeout: Duration) -> rusqlite::Result<String> {
    // A DELETE -> WAL lock upgrade can return SQLITE_BUSY immediately without
    // invoking SQLite's busy handler. Non-CLI readers may still hold this lock.
    // Use one deadline, rather than stacking each attempt's 30-second timeout.
    db.busy_timeout(Duration::ZERO)?;
    let deadline = Instant::now() + timeout;
    let result = loop {
        match db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)) {
            Err(e)
                if matches!(
                    &e,
                    rusqlite::Error::SqliteFailure(code, _)
                        if code.code == rusqlite::ErrorCode::DatabaseBusy
                ) =>
            {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break Err(e);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            result => break result,
        }
    };
    let restored = db.busy_timeout(LOCK_TIMEOUT);
    // Preserve the original SQLite error on failure, including non-BUSY errors.
    match result {
        Err(e) => Err(e),
        Ok(mode) => restored.map(|()| mode),
    }
}

fn open(path: &Path, initialize: bool) -> Result<Database> {
    let (path, operation_lock) = prepare_path(path, initialize)?;
    let mut db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(LOCK_TIMEOUT)?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
    // Recheck identity under the writer lock so concurrent initializations cannot reset it.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let app_id: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if app_id == 0 && version == 0 && initialize {
        let tables: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )?;
        if tables != 0 {
            return Err(error(
                "schema_mismatch",
                "Refusing to initialize another database.",
            ));
        }
        tx.execute_batch(
            "CREATE TABLE budget(
               singleton INTEGER PRIMARY KEY CHECK(singleton=1),
               budget_id TEXT NOT NULL, limit_micro_usd INTEGER NOT NULL CHECK(limit_micro_usd>0),
               closed INTEGER NOT NULL CHECK(closed IN (0,1)),
               halt_reason TEXT, created_at_ms INTEGER NOT NULL);
             CREATE TABLE requests(
               request_id TEXT PRIMARY KEY, request_sha256 TEXT NOT NULL,
               run_id TEXT NOT NULL, configuration_id TEXT NOT NULL,
               provider_scope TEXT NOT NULL, operation TEXT NOT NULL,
               price_snapshot_id TEXT NOT NULL, stage TEXT NOT NULL, ledger TEXT NOT NULL,
               bound_micro_usd INTEGER NOT NULL CHECK(bound_micro_usd>0),
               state TEXT NOT NULL CHECK(state IN ('reserved','dispatched','settled','cancelled')),
               actual_micro_usd INTEGER CHECK(actual_micro_usd>=0),
               cost_event_id TEXT UNIQUE, provider_request_id TEXT,
               created_at_ms INTEGER NOT NULL, dispatched_at_ms INTEGER, completed_at_ms INTEGER,
               UNIQUE(provider_scope,provider_request_id),
               CHECK((state='settled' AND actual_micro_usd IS NOT NULL
                     AND cost_event_id IS NOT NULL AND provider_request_id IS NOT NULL)
                     OR (state!='settled' AND actual_micro_usd IS NULL
                     AND cost_event_id IS NULL AND provider_request_id IS NULL)));
             CREATE TABLE audit(
               sequence INTEGER PRIMARY KEY AUTOINCREMENT,
               operation TEXT NOT NULL, request_id TEXT, recorded_at_ms INTEGER NOT NULL);
             PRAGMA application_id=1380729412;
             PRAGMA user_version=1;",
        )?;
    } else if app_id != APPLICATION_ID || version != SCHEMA {
        return Err(error(
            "schema_mismatch",
            "Not a supported Benchmark budget database.",
        ));
    }
    tx.commit()?;
    let journal = enable_wal(&db, LOCK_TIMEOUT)?;
    if journal != "wal" {
        return Err(error(
            "durability_unavailable",
            "SQLite WAL mode is required.",
        ));
    }
    db.execute_batch("PRAGMA synchronous=FULL;")?;
    Ok(Database {
        connection: db,
        _operation_lock: operation_lock,
    })
}

#[derive(Clone)]
struct Budget {
    id: String,
    limit: i64,
    closed: bool,
    halt: Option<String>,
}

fn budget(db: &Connection) -> Result<Budget> {
    db.query_row(
        "SELECT budget_id,limit_micro_usd,closed,halt_reason FROM budget WHERE singleton=1",
        [],
        |r| {
            Ok(Budget {
                id: r.get(0)?,
                limit: r.get(1)?,
                closed: r.get(2)?,
                halt: r.get(3)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| error("not_initialized", "Initialize the budget before using it."))
}

fn require_active(b: &Budget) -> Result<()> {
    if b.halt.is_some() {
        return Err(error(
            "budget_halted",
            "An accounting overrun has halted this budget.",
        ));
    }
    if b.closed {
        return Err(error("budget_closed", "This budget is closed."));
    }
    Ok(())
}

#[derive(Default)]
struct Totals {
    // Individual amounts are i64. Wider accumulation must retain genuine costs
    // even when several already-dispatched calls each exceed their reservation.
    spent: u128,
    reserved: u128,
    dispatched: u128,
    settled_count: u64,
    reserved_count: u64,
    dispatched_count: u64,
    cancelled_count: u64,
}

impl Totals {
    fn committed(&self) -> u128 {
        self.spent + self.reserved + self.dispatched
    }
}

fn totals(db: &Connection) -> Result<Totals> {
    let mut output = Totals::default();
    let mut statement =
        db.prepare("SELECT state,bound_micro_usd,actual_micro_usd FROM requests")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let state: String = row.get(0)?;
        let bound: i64 = row.get(1)?;
        match state.as_str() {
            "reserved" => {
                output.reserved += bound as u128;
                output.reserved_count += 1;
            }
            "dispatched" => {
                output.dispatched += bound as u128;
                output.dispatched_count += 1;
            }
            "settled" => {
                let actual: i64 = row.get(2)?;
                output.spent += actual as u128;
                output.settled_count += 1;
            }
            "cancelled" => output.cancelled_count += 1,
            _ => return Err(error("corrupt_ledger", "Unrecognized reservation state.")),
        }
    }
    Ok(output)
}

fn snapshot(db: &Connection) -> Result<Value> {
    let b = budget(db)?;
    let t = totals(db)?;
    let status = if b.halt.is_some() {
        "halted"
    } else if b.closed {
        "closed"
    } else if t.committed() >= b.limit as u128 {
        "exhausted"
    } else {
        "active"
    };
    Ok(json!({
        "budget_id":b.id, "schema_version":SCHEMA, "currency":"USD",
        "scope":"model_calls_only", "money_encoding":"decimal_integer_micro_usd",
        "status":status, "closed":b.closed, "halt_reason":b.halt,
        "limit_micro_usd":b.limit.to_string(),
        "settled_micro_usd":t.spent.to_string(),
        "reserved_micro_usd":t.reserved.to_string(),
        "dispatched_unsettled_micro_usd":t.dispatched.to_string(),
        "committed_micro_usd":t.committed().to_string(),
        "available_micro_usd":(b.limit as u128).saturating_sub(t.committed()).to_string(),
        "settled_requests":t.settled_count,"reserved_requests":t.reserved_count,
        "dispatched_unsettled_requests":t.dispatched_count,"cancelled_requests":t.cancelled_count,
        "accounting_status":if t.dispatched_count>0 { "unsettled" } else { "reconciled_recorded_calls" },
        "provider_invoice_cap_enforced":false
    }))
}

fn request(db: &Connection, id: &str) -> Result<Option<Value>> {
    Ok(db
        .query_row(
            "SELECT request_id,request_sha256,run_id,configuration_id,provider_scope,operation,
                price_snapshot_id,stage,ledger,bound_micro_usd,state,actual_micro_usd,
                cost_event_id,provider_request_id,created_at_ms,dispatched_at_ms,completed_at_ms
         FROM requests WHERE request_id=?1",
            [id],
            |r| {
                let bound: i64 = r.get(9)?;
                let actual: Option<i64> = r.get(11)?;
                Ok(json!({
                    "request_id":r.get::<_,String>(0)?, "request_sha256":r.get::<_,String>(1)?,
                    "run_id":r.get::<_,String>(2)?, "configuration_id":r.get::<_,String>(3)?,
                    "provider_scope":r.get::<_,String>(4)?, "operation":r.get::<_,String>(5)?,
                    "price_snapshot_id":r.get::<_,String>(6)?, "stage":r.get::<_,String>(7)?,
                    "ledger":r.get::<_,String>(8)?, "bound_micro_usd":bound.to_string(),
                    "state":r.get::<_,String>(10)?, "actual_micro_usd":actual.map(|n|n.to_string()),
                    "cost_event_id":r.get::<_,Option<String>>(12)?,
                    "provider_request_id":r.get::<_,Option<String>>(13)?,
                    "created_at_ms":r.get::<_,i64>(14)?,
                    "dispatched_at_ms":r.get::<_,Option<i64>>(15)?,
                    "completed_at_ms":r.get::<_,Option<i64>>(16)?
                }))
            },
        )
        .optional()?)
}

fn known_request(db: &Connection, id: &str) -> Result<Value> {
    identifier(id)?;
    request(db, id)?.ok_or_else(|| error("unknown_request", "No reservation has this request ID."))
}

fn audit(db: &Connection, operation: &str, request_id: Option<&str>) -> Result<()> {
    db.execute(
        "INSERT INTO audit(operation,request_id,recorded_at_ms) VALUES(?1,?2,?3)",
        params![operation, request_id, now_ms()?],
    )?;
    Ok(())
}

fn reserve(db: &Connection, args: &Reserve) -> Result<bool> {
    for field in [
        &args.request_id,
        &args.run_id,
        &args.provider_scope,
        &args.operation,
        &args.price_snapshot_id,
    ] {
        identifier(field)?;
    }
    sha256(&args.request_sha256)?;
    sha256(&args.configuration_id)?;
    let bound = money(&args.bound_usd, true)?;
    if let Some(previous) = request(db, &args.request_id)? {
        for (field, expected) in [
            ("request_sha256", args.request_sha256.as_str()),
            ("run_id", args.run_id.as_str()),
            ("configuration_id", args.configuration_id.as_str()),
            ("provider_scope", args.provider_scope.as_str()),
            ("operation", args.operation.as_str()),
            ("price_snapshot_id", args.price_snapshot_id.as_str()),
            ("stage", args.stage.as_str()),
            ("ledger", args.ledger.as_str()),
            ("bound_micro_usd", bound.to_string().as_str()),
        ] {
            if previous[field].as_str() != Some(expected) {
                return Err(error(
                    "idempotency_conflict",
                    "This request ID already exists with different parameters.",
                ));
            }
        }
        return Ok(true);
    }
    let b = budget(db)?;
    require_active(&b)?;
    if totals(db)?.committed() + bound as u128 > b.limit as u128 {
        return Err(error(
            "budget_exhausted",
            "The requested bound exceeds available budget.",
        ));
    }
    db.execute(
        "INSERT INTO requests(request_id,request_sha256,run_id,configuration_id,provider_scope,
             operation,price_snapshot_id,stage,ledger,bound_micro_usd,state,created_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'reserved',?11)",
        params![
            args.request_id,
            args.request_sha256,
            args.run_id,
            args.configuration_id,
            args.provider_scope,
            args.operation,
            args.price_snapshot_id,
            args.stage,
            args.ledger,
            bound,
            now_ms()?
        ],
    )?;
    audit(db, "reserve", Some(&args.request_id))?;
    Ok(false)
}

fn settle(db: &Connection, id: &str, actual: &str, event: &str, provider_id: &str) -> Result<bool> {
    identifier(event)?;
    identifier(provider_id)?;
    let actual = money(actual, false)?;
    let previous = known_request(db, id)?;
    if previous["state"] == "settled" {
        let actual_text = actual.to_string();
        if previous["actual_micro_usd"].as_str() == Some(actual_text.as_str())
            && previous["cost_event_id"] == event
            && previous["provider_request_id"] == provider_id
        {
            return Ok(true);
        }
        return Err(error(
            "idempotency_conflict",
            "This request already has a different final settlement.",
        ));
    }
    if previous["state"] != "dispatched" {
        return Err(error(
            "invalid_transition",
            "Only a dispatched request can settle.",
        ));
    }
    let duplicate_event: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM requests WHERE cost_event_id=?1)",
        [event],
        |r| r.get(0),
    )?;
    if duplicate_event {
        return Err(error(
            "duplicate_cost_event",
            "This cost event is already recorded.",
        ));
    }
    let duplicate_provider: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM requests WHERE provider_scope=?1 AND provider_request_id=?2)",
        params![previous["provider_scope"].as_str().unwrap(), provider_id],
        |r| r.get(0),
    )?;
    if duplicate_provider {
        return Err(error(
            "duplicate_provider_request",
            "This provider request is already settled in this billing scope.",
        ));
    }
    db.execute(
        "UPDATE requests SET state='settled',actual_micro_usd=?2,cost_event_id=?3,
            provider_request_id=?4,completed_at_ms=?5 WHERE request_id=?1",
        params![id, actual, event, provider_id, now_ms()?],
    )?;
    let bound: i64 = db.query_row(
        "SELECT bound_micro_usd FROM requests WHERE request_id=?1",
        [id],
        |r| r.get(0),
    )?;
    if actual > bound {
        db.execute(
            "UPDATE budget SET halt_reason=COALESCE(halt_reason,'settlement_exceeded_reservation')
             WHERE singleton=1",
            [],
        )?;
        audit(db, "overrun_halt", Some(id))?;
    }
    audit(db, "settle", Some(id))?;
    Ok(false)
}

fn run(cli: Cli) -> Result<Value> {
    // Reject bad initialization amounts before creating any files.
    if let Command::Init {
        budget_id,
        limit_usd,
    } = &cli.command
    {
        identifier(budget_id)?;
        money(limit_usd, true)?;
    }
    let mut db = open(&cli.db, matches!(cli.command, Command::Init { .. }))?;
    // Every mutation checks its prerequisites and commits under the same lock.
    // Snapshot also holds this lock to make totals and request state coherent.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut idempotent = false;
    let mut selected_id = None;
    let mut dispatch_permit = None;
    let operation = match &cli.command {
        Command::Init {
            budget_id,
            limit_usd,
        } => {
            let limit = money(limit_usd, true)?;
            let initialized: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM budget)", [], |r| r.get(0))?;
            if initialized {
                let b = budget(&tx)?;
                if b.id != *budget_id || b.limit != limit {
                    return Err(error(
                        "initialization_conflict",
                        "Existing budget identity and limit cannot be changed or reset.",
                    ));
                }
                idempotent = true;
            } else {
                tx.execute(
                    "INSERT INTO budget VALUES(1,?1,?2,0,NULL,?3)",
                    params![budget_id, limit, now_ms()?],
                )?;
                audit(&tx, "init", None)?;
            }
            "init"
        }
        Command::Reserve(args) => {
            budget(&tx)?;
            idempotent = reserve(&tx, args)?;
            selected_id = Some(args.request_id.as_str());
            "reserve"
        }
        Command::MarkDispatched { request_id } => {
            let b = budget(&tx)?;
            let previous = known_request(&tx, request_id)?;
            if previous["state"] == "reserved" {
                require_active(&b)?;
                tx.execute(
                    "UPDATE requests SET state='dispatched',dispatched_at_ms=?2 WHERE request_id=?1",
                    params![request_id,now_ms()?],
                )?;
                audit(&tx, "mark_dispatched", Some(request_id))?;
                dispatch_permit = Some(true);
            } else if previous["state"] == "dispatched" || previous["state"] == "settled" {
                idempotent = true;
                // A lost response or crashed caller must reconcile, never send again.
                dispatch_permit = Some(false);
            } else {
                return Err(error(
                    "invalid_transition",
                    "A cancelled request cannot dispatch.",
                ));
            }
            selected_id = Some(request_id.as_str());
            "mark_dispatched"
        }
        Command::Settle {
            request_id,
            actual_usd,
            cost_event_id,
            provider_request_id,
        } => {
            budget(&tx)?;
            idempotent = settle(
                &tx,
                request_id,
                actual_usd,
                cost_event_id,
                provider_request_id,
            )?;
            selected_id = Some(request_id.as_str());
            "settle"
        }
        Command::Cancel { request_id } => {
            budget(&tx)?;
            let previous = known_request(&tx, request_id)?;
            if previous["state"] == "cancelled" {
                idempotent = true;
            } else if previous["state"] == "reserved" {
                tx.execute(
                    "UPDATE requests SET state='cancelled',completed_at_ms=?2 WHERE request_id=?1",
                    params![request_id, now_ms()?],
                )?;
                audit(&tx, "cancel", Some(request_id))?;
            } else {
                return Err(error(
                    "invalid_transition",
                    "A dispatched request cannot be cancelled: unknown charges must remain held.",
                ));
            }
            selected_id = Some(request_id.as_str());
            "cancel"
        }
        Command::Close | Command::Reopen => {
            let b = budget(&tx)?;
            let closed = matches!(cli.command, Command::Close);
            idempotent = b.closed == closed;
            let operation = if closed { "close" } else { "reopen" };
            if !idempotent {
                tx.execute("UPDATE budget SET closed=?1 WHERE singleton=1", [closed])?;
                audit(&tx, operation, None)?;
            }
            operation
        }
        Command::Snapshot { request_id } => {
            if let Some(id) = request_id {
                known_request(&tx, id)?;
                selected_id = Some(id.as_str());
            }
            "snapshot"
        }
    };
    let mut response = json!({
        "status":"ok", "operation":operation, "idempotent":idempotent,
        "budget":snapshot(&tx)?
    });
    if let Some(id) = selected_id {
        response["request"] = known_request(&tx, id)?;
    }
    if let Some(permit) = dispatch_permit {
        response["dispatch_permit_granted"] = json!(permit);
    }
    tx.commit()?;
    Ok(response)
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                print!("{e}");
                return;
            }
            eprintln!(
                "{}",
                json!({"status":"error","error":{"code":"invalid_arguments","message":e.to_string()}})
            );
            std::process::exit(2);
        }
    };
    match run(cli) {
        Ok(response) => println!("{response}"),
        Err(e) => {
            eprintln!(
                "{}",
                json!({"status":"error","error":{"code":e.code,"message":e.message}})
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rollback_database() -> (tempfile::TempDir, Connection, Connection) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wal-upgrade.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE example(value INTEGER)")
            .unwrap();
        let holder = Connection::open(&path).unwrap();
        (directory, db, holder)
    }

    #[test]
    fn wal_upgrade_retries_busy_lock_upgrade_then_restores_busy_timeout() {
        let (_directory, db, holder) = rollback_database();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        db.busy_timeout(LOCK_TIMEOUT).unwrap();
        // This lock-upgrade conflict bypasses SQLite's normal busy handler.
        let original = db
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap_err();
        assert_eq!(
            original.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy)
        );
        let releasing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            holder.execute_batch("ROLLBACK").unwrap();
        });
        assert_eq!(enable_wal(&db, Duration::from_secs(2)).unwrap(), "wal");
        releasing.join().unwrap();
        assert_eq!(
            db.query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            30_000
        );
    }

    #[test]
    fn wal_upgrade_deadline_preserves_busy_error_and_restores_timeout() {
        let (_directory, db, holder) = rollback_database();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let started = Instant::now();
        let err = enable_wal(&db, Duration::from_millis(40)).unwrap_err();
        assert_eq!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy)
        );
        assert_eq!(err.to_string(), "database is locked");
        assert!(started.elapsed() >= Duration::from_millis(40));
        // Detect accidentally retaining a 30s busy handler per attempt.
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            db.query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            30_000
        );
    }

    #[test]
    fn wal_upgrade_does_not_retry_non_busy_errors() {
        let (directory, db, holder) = rollback_database();
        drop(holder);
        drop(db);
        let read_only = Connection::open_with_flags(
            directory.path().join("wal-upgrade.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let started = Instant::now();
        let err = enable_wal(&read_only, Duration::from_secs(2)).unwrap_err();
        assert_eq!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::ReadOnly));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn operation_lock_has_bounded_wait_and_survives_sqlite_close() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("budget.sqlite");
        let db = open(&path, true).unwrap();
        assert_eq!(
            db.query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2 // FULL on the actual CLI connection
        );
        let started = Instant::now();
        let err = acquire_operation_lock(&path, Duration::from_millis(40)).unwrap_err();
        assert_eq!(err.code, "operation_lock_timeout");
        assert!(started.elapsed() >= Duration::from_millis(40));
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(db);
        let _lock = acquire_operation_lock(&path, Duration::from_millis(40)).unwrap();
        assert!(!directory.path().join("budget.sqlite-wal").exists());
        assert!(!directory.path().join("budget.sqlite-shm").exists());
        assert!(directory
            .path()
            .join("budget.sqlite.operation.lock")
            .exists());
    }
}
