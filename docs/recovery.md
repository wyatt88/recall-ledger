# Backup, recovery, and managed Markdown projections

These operations use the local Rust store. They do not call a model, contact a
cloud service, edit an agent's instruction files, or change which database a
running daemon uses.

## Consistent SQLite backups

Create a new backup file in an existing directory:

```sh
mkdir -p .local-state/backups
chmod 700 .local-state/backups

recall-ledger --db .local-state/memory.sqlite3 \
  backup --destination .local-state/backups/snapshot.sqlite3
```

The backup uses SQLite's online backup API. Committed data in an active WAL is
included in a consistent snapshot; copying only a live `.db` file does not have
the same guarantee. A busy copy has a bounded retry interval and can fail with a
retryable error.

The implementation writes a private staging file, validates the current schema
and SQLite integrity, flushes the file, and publishes it without replacing an
existing destination. Both the main path and its `-wal`, `-shm`, and `-journal`
siblings must be absent; unknown sidecars are never removed or reused.
Unix files are created with mode `0600`. The receipt
contains the snapshot commit token, schema version, and file SHA-256; it does not
include record bodies.

A backup contains the database's namespaces, history, pending candidates,
accepted facts, dependency edges, vectors, jobs, and receipts at that point in
time. It is sensitive data and is not encrypted by this feature. Retraction in a
live store does not erase previously created backups.

Paths containing `..`, symbolic-link targets or user-created symbolic-link
components are rejected. macOS's OS-managed `/var`, `/tmp`, and `/etc` aliases
are resolved to `/private`. Containing directories must be trusted against
renaming by another process with the same operating-system identity.

## Restore to a new file and storage history

```sh
recall-ledger restore \
  --backup .local-state/backups/snapshot.sqlite3 \
  --destination .local-state/recovered.sqlite3
```

The destination must be new. The operation opens the source read-only, copies
and validates it, and changes only the new database:

- A fresh storage UUID and sequence zero identify the restored history.
- Old operation receipts and change events are removed. Old commit tokens and
  change cursors are rejected by the new history.
- Prepared derivation jobs are invalidated.
- Leased maintenance tasks return to pending with incremented generation.
  Their captured inputs, worker, and lease token are cleared. Old workers cannot
  complete those leases.
- Pending ingestion candidates, event deduplication fingerprints, accepted
  records, stable fact bindings, and their other snapshot state are retained.

This is point-in-time recovery. Changes after the snapshot, including corrections
or retractions, are absent from it. Inspect and reconcile that gap before making
the recovered database authoritative. Recovery does not merge the live database
or switch a daemon's database automatically.

Stop or fence old hosts and workers before switching to the new database. Clear
host caches and old change cursors. Receipt-based exactly-once behavior does not
span the restore boundary: blindly resubmitting an old direct write can create
a new operation. Preserved ingestion event IDs continue to provide event
deduplication inside the restored snapshot.

Only the current schema is accepted. For an older backup, migrate a separate
working copy with the matching supported `Store::open` migration path, then
create a new backup from that copy. Do not migrate the original backup in place.
Schema checks compare table, foreign-key, unique-constraint, index, and
virtual-table definitions with the supported schema, then check SQLite integrity
and foreign-key references. These checks recognize database structure; they do
not authenticate who produced a backup or prove its content is true.

After restoring, create a new projection directory. A directory bound to the
previous storage identity fails with `commit_history_mismatch`.

## Managed Markdown data views

Create or refresh a dedicated directory:

```sh
recall-ledger --db .local-state/memory.sqlite3 --namespace example \
  project --directory .local-state/projection

recall-ledger --db .local-state/memory.sqlite3 --namespace example \
  read-projection --directory .local-state/projection
```

The projection reads eligible records in one SQLite transaction, including
revision, dependency, namespace, and temporal checks. It holds the write
reservation until the pointer update is complete. Concurrent managed database
writes therefore cannot change that snapshot during its publication.

The directory has this structure:

```text
projection/
  .recall-ledger.lock
  CURRENT
  gen-<uuid>/
    MEMORY.md
    manifest.json
```

Each new generation is written once. Its manifest includes the storage identity,
namespace, commit token, record IDs and revisions, per-record body hashes, and
the Markdown hash. `CURRENT` contains the generation name and manifest hash.
Files and directories are flushed before an atomic `CURRENT` replacement makes
the generation visible. The reader takes a shared lock; the writer takes an
exclusive lock. Contention returns retryable `projection_busy`.

The default read command, backed by `Store::read_current_projection`, resolves
the pointer, verifies hashes, and checks the live database in one read
transaction. It requires the same namespace, storage identity, commit token,
eligible record IDs, revisions, and bodies. It also rechecks time-based expiry.
Stale output fails with `projection_stale` and returns no Markdown body. A
mutation in another namespace can also invalidate the global commit token;
refreshing is deliberately conservative.

An explicit offline audit is available without a database:

```sh
recall-ledger read-projection --directory .local-state/projection \
  --offline-integrity-only
```

This mode uses `projection::read_projection` and verifies hashes only. It can
return text that has since expired or been retracted in the live database, and
must not be used as a freshness check. Do not select a generation by directory
sort order. An unfinished unpublished generation is ignored. SHA-256 checks
detect accidental changes; they are not a digital signature or protection
against someone who can rewrite both data and checksums.

The writer refuses a changed current body, manifest, pointer, namespace, or
storage identity. It also refuses unmanaged files and symlinks. It never updates
`AGENTS.md`, `CLAUDE.md`, or any host's native memory file. To preserve an intentional
manual edit, copy it outside the managed directory and use a new directory.

All projected text is labeled `retrieved_data`. Body text is placed in Markdown
code fences, including text that itself contains backticks. This formatting does
not authorize tools or replace a host's prompt-injection defenses.

## Freshness and deletion boundaries

A projection remains a point-in-time file copy. A later write or time-based
expiry does not automatically rewrite `CURRENT`. The default reader rejects a
stale copy; refresh and read again to obtain current memory. Its guarantee is
the database snapshot checked during that read, not a promise that returned
content will stay current. A host can use change events to request refreshes,
but must also handle time-based expiry without a mutation event. Directly
reading `MEMORY.md` bypasses the live gate.

After a successful refresh, the current pointer cannot refer to the previous
generation. Previously completed generations are checked and removed on a
best-effort basis. Cleanup first atomically retires an old generation to a
`.gc-<uuid>` directory, then removes its files. A crash during this deletion
cannot leave a partially deleted `gen-<uuid>` that blocks future refreshes.
The result reports any retained unreferenced generation and ignored incomplete
entry. A crash after publication but before cleanup may leave a prior copy.
Incomplete unpublished and retired cleanup directories are left untouched and
never served.

Retraction followed by refresh removes affected records from the new managed
view; it does not retract a copy already supplied to a host. Historical copies,
failed-cleanup remnants, backups, filesystem snapshots, WAL pages, and storage
media are not securely erased by these operations.

The Markdown file limit is 64 MiB and the manifest limit is 8 MiB. Projection
construction still reads the eligible namespace and can consume database work
and memory proportional to its size; these file limits are not a large-store
performance guarantee.

## Verification

The deterministic integration tests use only temporary files and synthetic
content:

```sh
cargo test --locked --test recovery --test projection
```

They cover active-WAL backup, point-in-time data, restored identity and receipt
behavior, stale worker rejection, candidate/fact preservation, namespace binding,
live reads after revisions, retractions and expiry without a commit, managed
refresh and cleanup, manual edits, interrupted generations, path traversal,
symlinks, and lock contention. Regression cases explicitly reject existing
SQLite sidecars, removed schema constraints, and tolerate a retired cleanup
directory with only a manifest remaining. They do not simulate power loss or
prove all filesystem crash states, hostile same-user races, secure erasure, or
memory quality for a language model.

Independent local review reproduced and then rechecked three repaired defects:
an existing valid WAL could previously overwrite restored data on first open;
removed foreign-key definitions could pass the earlier column-only schema
check; and interrupted deletion of an old generation could prevent future
refreshes. The original WAL and missing-foreign-key cases now fail before a
destination is published. A simulated `.gc-<uuid>` remainder with only its
manifest no longer blocks refresh or the live reader. This is controlled local
validation, not a real power-loss test.
