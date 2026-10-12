# Claude Code: lifecycle memory adapter

RecallLedger 0.3 includes an installable project adapter for macOS and Linux.
The Rust daemon remains the authoritative store. A Python standard-library
command hook refreshes context before each submitted prompt and captures bounded
messages as candidates. A separate, explicit decision admits a candidate.

This is a supported-hook integration, not a replacement of Claude Code's internal
storage implementation. MCP alone does not redirect native Markdown reads or
writes. The adapter disables native auto memory for its chosen project and uses
hooks instead. It preserves `CLAUDE.md`, rules, existing hooks and native files.
Neither disabling auto memory nor withdrawing a record erases conversation text
that Claude has already received.

## Prerequisites and installation

Use a local RecallLedger 0.3 daemon. The Python SDK requires Python 3.9 or later,
Unix-domain sockets and `fcntl`.

From this repository, build and install in an environment that will stay at the
same path:

```sh
cargo build --release
python3 -m venv .venv
.venv/bin/python -m pip install ./sdk/python
```

Start a daemon with its namespace fixed at startup. Keep the socket path short
enough for your operating system:

```sh
mkdir -p /absolute/private/state
chmod 700 /absolute/private/state
./target/release/recall-ledger \
  --db /absolute/private/state/memory.sqlite3 --namespace example-project \
  serve --socket /absolute/private/state/memory.sock --no-worker
```

In another terminal, explicitly choose the project:

```sh
.venv/bin/recall-ledger-claude install \
  --project /absolute/path/to/isolated-project \
  --socket /absolute/private/state/memory.sock
```

The installer modifies only that project's `.claude/settings.local.json` and
creates `.claude/recall-ledger/`. Existing hooks and unrelated settings are
preserved. It sets `autoMemoryEnabled: false`, appends two command hooks and saves
a private before/after receipt with hashes. It does not modify `~/.claude`, edit
native memory, register a service, launch Claude, or test daemon health.

Generated hooks use the Python executable that ran the installer:

```json
{
  "autoMemoryEnabled": false,
  "hooks": {
    "UserPromptSubmit": [{
      "hooks": [{
        "type": "command",
        "command": "/absolute/venv/bin/python -m recall_ledger.claude_code hook --project /absolute/project",
        "timeout": 16
      }]
    }],
    "Stop": [{
      "hooks": [{
        "type": "command",
        "command": "/absolute/venv/bin/python -m recall_ledger.claude_code hook --project /absolute/project",
        "timeout": 16
      }]
    }]
  }
}
```

This is an illustrative fragment, not a file to overwrite existing settings
with. The installer quotes actual executable/project paths, including spaces.
Use `/hooks` in a newly opened Claude session to inspect the effective settings.
Claude's workspace-trust and managed-policy requirements still apply; the
installer cannot override a host that blocks local hooks.

The adapter state contains captured text and configuration snapshots. Exclude
`.claude/recall-ledger/` and `.claude/settings.local.json` from version control
and shared archives. Files created by the adapter are mode `0600`, and its state
directory is `0700`. These permissions do not stop processes running as the same
operating-system user.

Installation options:

| Option | Default | Behavior |
| --- | --- | --- |
| `--mode` | `lexical` | `lexical`, `hybrid` or `vector`; semantic modes need configured local embedding services |
| `--timeout` | `10` seconds | Shared IPC deadline for capture delivery and recall; settings allow an extra six seconds for locking/startup |
| `--max-chars` | `4000` | Recall body budget, 128–16000 Unicode characters |
| `--capture-chars` | `8000` | Per-message candidate budget, 256–16000 characters; longer messages have an explicit truncation marker |
| `--no-capture-user` | off | Refresh context without capturing prompt envelopes |
| `--no-capture-assistant` | off | Do not capture the final assistant response |

Default lexical recall and a daemon without model configuration require no
model service. Capture makes no inference calls. Configured semantic retrieval
or the daemon's optional maintenance worker can invoke your local model service.

## What happens during a turn

1. `UserPromptSubmit` queues a bounded prompt candidate with origin
   `user_message`. The source label is fixed as `claude-code:user_message`.
2. The hook attempts to flush at most eight outbox entries, reserving half of its
   IPC budget for retrieval. New candidates are `pending`; they do not enter
   recall, vector indexing or summary discovery.
3. It calls `recall` using the current prompt, with the previous session receipt
   as `minimum_commit`. The query is capped at 4096 UTF-8 bytes.
4. It injects the current items, coverage and daemon-provided context manifest
   in `hookSpecificOutput.additionalContext`. The packet explicitly identifies
   bodies as untrusted data. Only IDs, revisions and receipts are cached locally
   for context refresh; retrieved bodies are not cached.
5. `Stop` queues `last_assistant_message` as `model_inference` and flushes a
   bounded batch. It returns no decision and no additional context, so the
   adapter does not intentionally continue the model's stop loop.

`user_message` is an envelope provenance label, not proof that a human authored
or endorsed the text. Current Claude documentation says the same event can fire
for scheduled or background turns, and its documented input does not provide an
authenticated author field. No origin is an automatic fact or approval.

The adapter does not capture every tool result, subagent turn, hidden reasoning,
or interrupted response. It performs no learned extraction or semantic conflict
resolution. Its first version deliberately submits bounded text for review.
For structured applications, the SDK can submit a precise `fact_key`; see
[candidate intake](ingestion.md).

## Review and explicitly admit candidates

```sh
recall-ledger-claude candidates --project /absolute/path/to/isolated-project
recall-ledger-claude inspect --project /absolute/path/to/isolated-project \
  --candidate-id CANDIDATE_ID
recall-ledger-claude resolve --project /absolute/path/to/isolated-project \
  --candidate-id CANDIDATE_ID --decision accept
```

Use `--decision reject` to reject instead. Candidate listing returns metadata,
not bodies; inspect a candidate before resolving it. Resolution is an explicit
API/CLI action, not human authentication: another process with daemon access can
also call it. There is no automatic approval policy in this adapter.

For an existing keyed fact, pass its current `--expected-revision`. Conflicts
leave the proposal pending. The CLI derives a stable review key from candidate,
decision and expected revision; `--idempotency-key` can explicitly pin another
key. Never change payloads while reusing a key.

The SDK exposes the same operations:

```python
from recall_ledger import Client

memory = Client("/absolute/private/state/memory.sock", timeout=10)
proposal = memory.candidate_submit(
    "The project uses Rust.",
    event_id="my-host:session-42:message-7",
    origin="user_message",
    source="my-host:project-notes",
    fact_key={
        "entity": "project:example",
        "attribute": "implementation_language",
        "scope": "repository:example",
        "conditions": "default build",
    },
    idempotency_key="submit:session-42:message-7",
)
# Inspect and obtain a trusted explicit decision before resolving.
review = memory.candidate_inspect(proposal["candidate_id"])
```

`candidate_list`, `candidate_resolve`, `fact_get`, `changes` and
`validate_context` are also available on `Client`. SDK calls do not automatically
retry transport failures.

## Migrate existing Markdown before switching

Supply the directory explicitly; the adapter does not search a user's home
directory or guess Claude's escaped project name:

```sh
recall-ledger-claude migrate \
  --project /absolute/path/to/isolated-project \
  --socket /absolute/private/state/memory.sock \
  --directory /explicit/path/to/native-memory
recall-ledger-claude flush \
  --project /absolute/path/to/isolated-project \
  --socket /absolute/private/state/memory.sock
```

Migration can run before installation. It reads `.md` and `.mdc` files, proposes
chunks with origin `import` and fixed source `claude-code:import`, and records
private relative-path/file-hash provenance. It never edits or deletes source
files. Import receipts map every chunk's event ID to its original character
span; whitespace-only chunks are skipped. At most 1000 files are selected for
import, at most 2 MiB is read per file,
and the outbox holds at most 1000 entries. A large or failed import can be
partial: inspect the returned failures and queue count, flush, and rerun.

Repeat imports of unchanged files use the same event identity. Changed file
content creates new candidates; it does not silently replace accepted facts.
Markdown instructions are imported as untrusted text, not as rules. Review and
resolve the imported candidates before installing hooks if the switch should
start with usable memory.

The migration commands, `candidates`, `inspect` and `resolve` accept `--socket`
before installation. Once a project has a saved adapter configuration, a
different socket is refused to avoid silently crossing namespaces.

## Freshness, retries and failure behavior

The manifest identifies the namespace, commit token and exact returned record
revisions. Validate a session's most recent manifest with:

```sh
recall-ledger-claude validate \
  --project /absolute/path/to/isolated-project --session-id CLAUDE_SESSION_ID
```

Application code can call `validate_context(manifest)` immediately before using
a remembered fact. This adapter refreshes on submitted prompts; it does not
intercept every later action in the same agent turn. A successful check does not
lock the store against subsequent writes. Old conversation text remains in the
host, and a missing retrieval hit alone is not proof of deletion.

Restoring a database changes its history identity. The installed adapter refuses
that destination with `binding_mismatch`; it does not drop the old commit barrier,
automatically rebind, or replay old queued proposals. Re-running `install` while
the old installation is active retains its binding and does not bypass this check.

For a planned switch in the same project, stop the old host's hooks and background
writers, then run `recall-ledger-claude uninstall --project /absolute/project`
while the original bound daemon is still available. Preserve the old database and
privately archive `.claude/recall-ledger/` outside its active location, including
its queue, import receipts and session metadata. Reconcile changes after the
backup before making the restored database authoritative. Start its daemon at the
intended socket, run the normal `install --project ... --socket ...` command, and
start a new host session. Do not copy the old queue or session receipts into the
new installation; unresolved proposals need explicit review.

If the original bound daemon is unavailable, uninstall cannot prepare its verified
fallback and stops before changing settings. The current CLI has no forced
uninstall or `rebind` command; recovering the original service or reviewing a
manual migration is separate work. Neither path erases already delivered host
context.

Host event IDs use the project, session, event kind and full-text hash, with a
bounded transcript UUID hint when available. The first mapping is persisted
before delivery; retries reuse it even if the transcript later grows. The hook
schema does not guarantee an event UUID. Therefore identical text for the same
event kind in a session is intentionally coalesced. Transcript content is not
ingested; only the provided file's final 128 KiB can be read for the UUID hint.

Outbox entries preserve exact request arguments and idempotency keys after an
unknown transport outcome. A later hook or explicit `flush` retries them.
Successful delivery deletes the entry. Known permanent candidate errors move the
original request and machine-readable error code into private `dead-letter/`
storage, allowing later entries to proceed. Retryable errors, transport failures,
unknown methods and schema mismatches stop the batch and preserve the queue.
Quarantined entries do not automatically requeue on a repeated hook or import.

```sh
recall-ledger-claude dead-letters --project /absolute/path/to/isolated-project
recall-ledger-claude dead-letter-inspect \
  --project /absolute/path/to/isolated-project --entry ENTRY_HASH
recall-ledger-claude requeue \
  --project /absolute/path/to/isolated-project --entry ENTRY_HASH
recall-ledger-claude flush --project /absolute/path/to/isolated-project
```

Listing is metadata-only and capped at 100 entries with `truncated`; the private
`dead-letter/` directory retains every complete request. Inspect one explicitly
to read its body. Requeue preserves the exact payload, event ID and key; it is
useful after fixing an external cause, not as a way to change a rejected payload.
An invalid payload should be reviewed and deliberately submitted as a new event
through the SDK, not edited in place under an existing idempotency key.

A full outbox or capture failure is reported, not silently called durable.
Event receipts and captured outbox/dead-letter bodies remain private local
state; clearing daemon records does not erase external files or the host
transcript.

If the daemon is unavailable, recall injects an explicit unavailable/freshness
notice and no cached body; the user's prompt continues. Captured candidates stay
in the outbox. Invalid configuration or local hook errors also fail open with a
visible hook message. A host-enforced timeout can discard the hook's output
entirely, so fail-open operation is not a safety gate for consequential actions.

## Uninstall without reviving stale native memory

```sh
recall-ledger-claude uninstall --project /absolute/path/to/isolated-project
```

Uninstall checks that settings still match the installed hash. It exports
current readable records plus bounded pending metadata, checks the hash again,
removes this adapter's hooks by restoring the saved unrelated settings, and
**keeps `autoMemoryEnabled: false`**. A changed settings file or failed daemon
export stops uninstall without overwriting the user's settings. Resolve the
local difference or reconnect the daemon before retrying; there is no force
overwrite option.

An uninstall intent, target bytes/hashes and fallback hash are saved before the
settings write. If the process is interrupted, repeat the same uninstall command
and native-mode flag. A retry completes only when settings exactly match either
the installed state or its recorded restoration target. It uses the fallback
captured when that uninstall was prepared, not a new live snapshot.

The private fallback artifact is a dated-by-commit snapshot, not a live view.
Retracted/invalid records are absent, pending bodies remain in the daemon, and
pending metadata may be truncated. Preserve the database and adapter state
(including outbox and dead letters). Use the
daemon's [backup/restore](recovery.md) for actual recovery. The adapter does not
write the fallback into native Markdown or pretend that it becomes a native
backend.

Only after reviewing old native files and the current fallback, explicitly use:

```sh
recall-ledger-claude uninstall \
  --project /absolute/path/to/isolated-project --enable-native-after-review
```

That flag restores the original `autoMemoryEnabled` setting, including original
absence; it does not guarantee native memory is enabled because higher-priority
host settings or environment variables still apply. Existing native files are
never restored from an old copy or overwritten. To use the flag, choose it on the
active installation's uninstall operation; an already completed default
uninstall does not later change native mode. Hooks retained by an older host
session become inert after uninstall or while its saved intent is pending.

## Evidence and remaining host acceptance

Official references checked on 2026-10-10:

- [Hook locations and envelopes](https://code.claude.com/docs/en/hooks):
  project-local settings; `UserPromptSubmit` input/output; `Stop` final text and
  continuation behavior; command timeouts and host trust/policy restrictions.
- [Native memory](https://code.claude.com/docs/en/memory):
  `autoMemoryEnabled`, native Markdown storage and separate `CLAUDE.md` rules.
- [Settings scopes and precedence](https://code.claude.com/docs/en/settings).

These host mechanisms are **E2: official methods reviewed**. The implementation
has local synthetic tests of generated settings, retries, migration and errors,
plus real Rust-daemon subprocess checks of capture, pending isolation, explicit
acceptance, revision invalidation, withdrawal and uninstall. These are local
engineering tests, not a paper reproduction or Claude host acceptance.

Run all checks from the repository:

```sh
cargo build
PYTHONPATH=sdk/python RECALL_LEDGER_BIN="$PWD/target/debug/recall-ledger" \
  python3 -m unittest discover -s sdk/python/tests -v
```

Without `RECALL_LEDGER_BIN`, only the real-daemon test class is skipped. No test
calls a model or reads an existing private Claude configuration.

Still required for **E4**: record the exact installed Claude version, verify
workspace trust and effective `/hooks`/memory settings, run a real isolated
conversation that creates a pending proposal, explicitly accept it, then verify
a later turn sees a revision and no longer retrieves a withdrawn record.
Include offline behavior and uninstall in that host acceptance record. Fixture
success does not establish that the model follows injected data boundaries or
that memory improves task quality.
