//! File-oriented administrative migration. Review files pin both the source
//! and the destination; applying never silently refreshes a stale plan.
use clap::Subcommand;
use recall_ledger::{Error, Result, Store};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: usize = 512 * 1024;

#[derive(Subcommand)]
pub enum Command {
    /// Export one complete, bounded namespace to a new private JSON file.
    Export {
        #[arg(long)]
        output: PathBuf,
    },
    /// Validate a bundle and save a conflict preview pinned to this destination.
    Plan {
        #[arg(long)]
        bundle: PathBuf,
        /// Previously obtained namespace/history identity of the source.
        #[arg(long)]
        source_binding: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Apply an inspected plan atomically; changed destinations require a new plan.
    Apply {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        idempotency_key: String,
    },
}

fn failure(code: &str, message: &str) -> Error {
    crate::transport::failure(code, message, false)
}

fn read_json(path: &Path, limit: usize) -> Result<Value> {
    let fail = || failure("invalid_file", "Expected a bounded regular JSON file.");
    let meta = path.symlink_metadata().map_err(|_| fail())?;
    if !meta.is_file() || meta.len() > limit as u64 {
        return Err(fail());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|_| fail())?;
    if !file.metadata().map_err(|_| fail())?.is_file() {
        return Err(fail());
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| fail())?;
    if bytes.len() > limit {
        return Err(fail());
    }
    serde_json::from_slice(&bytes).map_err(|_| fail())
}

fn write_new_json(path: &Path, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| failure("invalid_file", "Cannot serialize the migration file."))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(failure(
            "resource_exhausted",
            "Migration file exceeds 512 KiB.",
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|_| {
        failure(
            "invalid_path",
            "Output must be a new file in an existing writable directory.",
        )
    })?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
        return Err(failure(
            "io_error",
            "Could not durably write the migration file.",
        ));
    }
    Ok(())
}

pub fn run(db: PathBuf, namespace: &str, command: Command) -> Result<Value> {
    match command {
        Command::Export { output } => {
            let mut store = Store::open(db)?;
            let bundle = store.execute(namespace, "bundle_export", json!({}))?;
            write_new_json(&output, &bundle)?;
            Ok(json!({
                "written":output,"digest":bundle["digest"],
                "source_binding":bundle["payload"]["source_binding"],
                "source_sequence":bundle["payload"]["source_sequence"]
            }))
        }
        Command::Plan {
            bundle,
            source_binding,
            output,
        } => {
            let bundle = read_json(&bundle, MAX_FILE_BYTES)?;
            let source = read_json(&source_binding, 4096)?;
            recall_ledger::validate_binding(&source)?;
            let mut store = Store::open(db)?;
            let target = store.execute(namespace, "binding", json!({}))?;
            let preview = store.execute_bound(
                namespace,
                "bundle_plan",
                json!({"bundle":bundle,"expected_source":source}),
                Some(&target),
            )?;
            let review = json!({
                "format":"recall-ledger.import-plan/1",
                "expected_source":source,
                "expected_target":target,
                "reviewed_digest":bundle["digest"],
                "expected_target_commit":preview["target_commit"],
                "preview":preview
            });
            write_new_json(&output, &review)?;
            Ok(json!({"written":output,"review":review}))
        }
        Command::Apply {
            bundle,
            plan,
            idempotency_key,
        } => {
            let bundle = read_json(&bundle, MAX_FILE_BYTES)?;
            let review = read_json(&plan, MAX_FILE_BYTES)?;
            let object = review.as_object().ok_or_else(|| {
                failure(
                    "invalid_plan",
                    "Expected a recall-ledger.import-plan/1 review.",
                )
            })?;
            if object.len() != 6
                || review["format"] != "recall-ledger.import-plan/1"
                || ![
                    "expected_source",
                    "expected_target",
                    "reviewed_digest",
                    "expected_target_commit",
                    "preview",
                ]
                .iter()
                .all(|key| object.contains_key(*key))
            {
                return Err(failure("invalid_plan", "Invalid migration review fields."));
            }
            recall_ledger::validate_binding(&review["expected_source"])?;
            recall_ledger::validate_binding(&review["expected_target"])?;
            if review["reviewed_digest"].as_str().is_none()
                || review["expected_target_commit"].as_str().is_none()
            {
                return Err(failure("invalid_plan", "Migration review is incomplete."));
            }
            Store::open(db)?.execute_bound(
                namespace,
                "bundle_apply",
                json!({
                    "bundle":bundle,
                    "expected_source":review["expected_source"],
                    "reviewed_digest":review["reviewed_digest"],
                    "expected_target_commit":review["expected_target_commit"],
                    "idempotency_key":idempotency_key
                }),
                Some(&review["expected_target"]),
            )
        }
    }
}
