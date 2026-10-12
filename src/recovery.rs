//! Consistent SQLite snapshots and restoration into a new storage history.
use super::*;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::OpenFlags;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, PathBuf};
use std::time::Instant;

pub(crate) fn io_error() -> Error {
    Error::new("io_error", "Managed filesystem operation failed.")
}

/// Resolve system aliases, but reject traversal and user-controlled symlink
/// components. The containing directories must be trusted against rename by
/// another process running under the same OS identity.
pub(crate) fn checked_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error::new(
            "invalid_path",
            "A nonempty path without parent traversal is required.",
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|_| io_error())?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => continue,
            Component::ParentDir => unreachable!(),
            _ => resolved.push(component.as_os_str()),
        }
        match fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                // macOS exposes these OS-managed aliases to /private. This
                // exception does not extend to user-created links below them.
                #[cfg(target_os = "macos")]
                if ["/var", "/tmp", "/etc"]
                    .iter()
                    .any(|alias| resolved == Path::new(alias))
                {
                    let canonical = resolved.canonicalize().map_err(|_| io_error())?;
                    let expected = Path::new("/private").join(resolved.strip_prefix("/").unwrap());
                    if canonical == expected {
                        resolved = canonical;
                        continue;
                    }
                }
                return Err(Error::new(
                    "invalid_path",
                    "Symbolic links are not accepted for managed paths.",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(io_error()),
        }
    }
    Ok(resolved)
}

pub(crate) fn private_create(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            Error::new("destination_exists", "Destination already exists.")
        } else {
            io_error()
        }
    })
}

pub(crate) fn regular_open(path: &Path, write: bool) -> Result<File> {
    let metadata = fs::symlink_metadata(path).map_err(|_| io_error())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Error::new(
            "invalid_path",
            "A regular file without symbolic links is required.",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|_| io_error())?;
    if !file.metadata().map_err(|_| io_error())?.is_file() {
        return Err(Error::new("invalid_path", "A regular file is required."));
    }
    Ok(file)
}

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|_| io_error())?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = private_create(path)?;
    file.write_all(bytes).map_err(|_| io_error())?;
    file.sync_all().map_err(|_| io_error())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = regular_open(path, false)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 32 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|_| io_error())?;
        if count == 0 {
            return Ok(format!("{:x}", digest.finalize()));
        }
        digest.update(&buffer[..count]);
    }
}

fn sqlite_open(path: &Path, write: bool) -> Result<Connection> {
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let db = Connection::open_with_flags(path, flags)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON;")?;
    Ok(db)
}

fn copy_snapshot(source: &Connection, destination: &mut Connection) -> Result<()> {
    let backup = Backup::new(source, destination)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match backup.step(256)? {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            StepResult::Busy | StepResult::Locked => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => return Err(Error::new("backup_failed", "Unexpected backup state.")),
        }
        if Instant::now() >= deadline {
            return Err(Error {
                code: "backup_busy".into(),
                message: "Backup could not finish within the bounded copy interval.".into(),
                retryable: true,
            });
        }
    }
}

// Preserve quoted text exactly while ignoring differences in the indentation
// of the supported CREATE statements. Definitions include foreign keys, unique
// constraints, indexes, defaults and virtual-table options, not only columns.
fn normalized_schema_sql(sql: &str) -> String {
    let mut result = String::new();
    let mut quote = None;
    let mut pending_space = false;
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(end) = quote {
            result.push(ch);
            if ch == end {
                if chars.peek() == Some(&end) {
                    result.push(chars.next().unwrap());
                } else {
                    quote = None;
                }
            }
        } else if ch.is_whitespace() {
            pending_space = !result.is_empty();
        } else {
            if pending_space {
                result.push(' ');
                pending_space = false;
            }
            result.push(ch);
            quote = match ch {
                '\'' | '"' | '`' => Some(ch),
                '[' => Some(']'),
                _ => None,
            };
        }
    }
    result
}

fn schema_inventory(db: &Connection) -> Result<BTreeMap<String, (String, Vec<String>)>> {
    let mut statement = db.prepare(
        "SELECT name,type,sql FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let objects = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut inventory = BTreeMap::new();
    for (name, object_type, sql) in objects {
        let mut detail = vec![normalized_schema_sql(sql.as_deref().unwrap_or_default())];
        if object_type == "table" {
            let mut columns = db.prepare(
                "SELECT name,type,\"notnull\",pk,hidden FROM pragma_table_xinfo(?1) ORDER BY cid",
            )?;
            for row in columns.query_map([&name], |row| {
                Ok(format!(
                    "{}|{}|{}|{}|{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?
                ))
            })? {
                detail.push(row?);
            }
        }
        inventory.insert(name, (object_type, detail));
    }
    Ok(inventory)
}

fn validate_database(db: &Connection) -> Result<()> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != SCHEMA {
        return Err(Error::new(
            "schema_mismatch",
            "Backup must use the current RecallLedger schema; migrate a separate copy first.",
        ));
    }
    let reference = Store::open(":memory:")?;
    if schema_inventory(db)? != schema_inventory(&reference.connection)? {
        return Err(Error::new(
            "schema_mismatch",
            "Database does not match the current RecallLedger schema.",
        ));
    }
    let id: String = db.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
        r.get(0)
    })?;
    let sequence: String =
        db.query_row("SELECT value FROM meta WHERE key='sequence'", [], |r| {
            r.get(0)
        })?;
    if Uuid::parse_str(&id).is_err() || sequence.parse::<u64>().is_err() {
        return Err(Error::new("schema_mismatch", "Invalid storage identity."));
    }
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(Error::new(
            "invalid_backup",
            "SQLite integrity check failed.",
        ));
    }
    let foreign_key_error = db
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some();
    if foreign_key_error {
        return Err(Error::new(
            "invalid_backup",
            "Database contains broken foreign-key references.",
        ));
    }
    Ok(())
}

struct StagedDatabase {
    temporary: PathBuf,
    destination: PathBuf,
}

fn sqlite_sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn require_new_database_path(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        match fs::symlink_metadata(sqlite_sibling(path, suffix)) {
            Ok(_) => {
                return Err(Error::new(
                    "destination_exists",
                    "Destination or an associated SQLite sidecar already exists.",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(io_error()),
        }
    }
    Ok(())
}

impl StagedDatabase {
    fn new(destination: &Path) -> Result<Self> {
        let destination = checked_path(destination)?;
        require_new_database_path(&destination)?;
        let parent = destination
            .parent()
            .filter(|p| p.is_dir())
            .ok_or_else(|| Error::new("invalid_path", "Destination parent must exist."))?;
        let temporary = parent.join(format!(".recall-ledger-backup-{}.partial", Uuid::new_v4()));
        require_new_database_path(&temporary)?;
        private_create(&temporary)?;
        Ok(Self {
            temporary,
            destination,
        })
    }

    fn publish(&self) -> Result<()> {
        regular_open(&self.temporary, false)?
            .sync_all()
            .map_err(|_| io_error())?;
        // A leftover WAL or rollback journal can replace the just-published
        // main database on its next open. Never remove an unknown sidecar.
        require_new_database_path(&self.destination)?;
        // Unlike rename, hard_link cannot replace an existing destination.
        fs::hard_link(&self.temporary, &self.destination).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::new("destination_exists", "Destination already exists.")
            } else {
                io_error()
            }
        })?;
        fs::remove_file(&self.temporary).map_err(|_| io_error())?;
        sync_directory(self.destination.parent().unwrap())
    }
}

impl Drop for StagedDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = fs::remove_file(sqlite_sibling(&self.temporary, suffix));
        }
    }
}

impl Store {
    /// Copy a consistent SQLite snapshot, including committed WAL contents.
    /// The destination must not already exist. No source bodies are returned.
    pub fn backup(&self, destination: impl AsRef<Path>) -> Result<Value> {
        let staged = StagedDatabase::new(destination.as_ref())?;
        let mut output = sqlite_open(&staged.temporary, true)?;
        copy_snapshot(&self.connection, &mut output)?;
        output.pragma_update(None, "journal_mode", "DELETE")?;
        validate_database(&output)?;
        let token = commit_token(&output)?;
        output.close().map_err(|(connection, error)| {
            drop(connection);
            Error::from(error)
        })?;
        let sha256 = hash_file(&staged.temporary)?;
        staged.publish()?;
        Ok(json!({
            "format":"recall-ledger.sqlite-backup/1",
            "schema_version":SCHEMA,
            "commit_token":token,
            "sha256":sha256,
            "consistent_snapshot":true,
            "restore_requires_new_path":true
        }))
    }

    /// Restore a validated snapshot into a new file and a new storage identity.
    /// This never replaces a running database or changes the source snapshot.
    pub fn restore(backup: impl AsRef<Path>, destination: impl AsRef<Path>) -> Result<Value> {
        let source_path = checked_path(backup.as_ref())?;
        regular_open(&source_path, false)?;
        let source = sqlite_open(&source_path, false)?;
        // Validate only the copied, consistent snapshot: the source can have an
        // active WAL writer, and separate pre-copy reads would not pin one state.
        let staged = StagedDatabase::new(destination.as_ref())?;
        let mut output = sqlite_open(&staged.temporary, true)?;
        copy_snapshot(&source, &mut output)?;
        output.pragma_update(None, "journal_mode", "DELETE")?;
        validate_database(&output)?;
        let source_commit = commit_token(&output)?;
        let new_id = Uuid::new_v4().to_string();
        let tx = output.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Reset transport history without inventing a new provenance identity
        // for old native records. Otherwise an old restored snapshot could
        // bypass tombstones already imported by another deployment.
        portable::freeze_native_origins(&tx)?;
        tx.execute("UPDATE meta SET value=?1 WHERE key='store_id'", [&new_id])?;
        tx.execute("UPDATE meta SET value='0' WHERE key='sequence'", [])?;
        let cleared_receipts = tx.execute("DELETE FROM operations", [])?;
        tx.execute("DELETE FROM change_events", [])?;
        let invalidated_jobs = tx.execute(
            "UPDATE jobs SET state='invalidated' WHERE state='prepared'",
            [],
        )?;
        tx.execute(
            "DELETE FROM maintenance_inputs
             WHERE task_id IN (SELECT id FROM maintenance_tasks WHERE state='leased')",
            [],
        )?;
        let reset_leases = tx.execute(
            "UPDATE maintenance_tasks SET state='pending',generation=generation+1,
             lease_token=NULL,lease_until=NULL,worker=NULL,output_revision=NULL,
             next_attempt_at=0 WHERE state='leased'",
            [],
        )?;
        let token = commit_token(&tx)?;
        tx.commit()?;
        validate_database(&output)?;
        output.close().map_err(|(connection, error)| {
            drop(connection);
            Error::from(error)
        })?;
        let sha256 = hash_file(&staged.temporary)?;
        staged.publish()?;
        Ok(json!({
            "format":"recall-ledger.restore/1","schema_version":SCHEMA,
            "source_commit_token":source_commit,"commit_token":token,
            "cleared_receipts":cleared_receipts,"invalidated_prepared_jobs":invalidated_jobs,
            "reset_leases":reset_leases,"sha256":sha256,"new_storage_history":true,
            "point_in_time":true
        }))
    }
}
