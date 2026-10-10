# Codex local hooks adapter

The adapter connects the local Codex harness to a locally running RecallLedger
daemon. It creates a reversible project configuration patch, captures proposed
memories, and retrieves fresh context for user turns and compaction. Installation
does not grant Codex hook trust or prove a live session has loaded the hooks.

Requirements: a built RecallLedger daemon, the Python SDK installed with
Python **3.11 or newer**, and a Codex version supporting the documented project
hooks and memory settings. The shared SDK still supports Python 3.9; this
installer uses Python 3.11's `tomllib` to verify configuration preservation.
Unix-domain sockets and file locks make this adapter Unix-only.

## Install in an explicitly selected project

Start a daemon with the database and namespace you intend to use:

```sh
recall-ledger --db /absolute/private/memory.db --namespace my-project \
  serve --socket /absolute/private/memory.sock --no-worker

python3 -m pip install ./sdk/python
recall-ledger-codex install \
  --project /absolute/project \
  --socket /absolute/private/memory.sock
```

The SDK must remain importable by the Python executable used at installation.
The generated command records that executable's absolute path. A project path
containing spaces is supported. Existing symlink components are refused.

The installer:

- Merges commands into `.codex/hooks.json`, retaining existing hook groups and
  other keys.
- Sets only `memories.generate_memories = false` and
  `memories.use_memories = false` in `.codex/config.toml`, checking that the
  complete parsed TOML is otherwise unchanged. An unsupported TOML layout fails
  before a configuration patch is applied.
- Saves private state under `.codex/recall-ledger/`, including a
  `namespace/history_id` binding, byte snapshots, and a recovery journal.

Review the generated definitions in Codex's `/hooks` interface and complete the
host's normal project/hook trust process. Start a new session and inspect its
effective hooks and memory settings. User-level and managed hooks can also
apply; this patch does not remove them. Its result explicitly reports
`host_trusted: false`, `effective_hooks_verified: false`, and
`live_session_verified: false` until those checks are performed outside the
installer. Those fields are not a persisted host attestation.

Add `.codex/recall-ledger/` to your project's ignore rules before publishing a
repository. It contains proposed memory text, configuration snapshots, and
uninstall exports. The adapter does not edit `.gitignore`.

## Lifecycle and admission

| Hook | Behavior |
| --- | --- |
| `UserPromptSubmit` | Captures the current prompt as a `user_message` candidate; flushes a bounded outbox batch; recalls using that prompt and the previous committed read token. |
| `SessionStart` with `source=compact` | Performs a fresh, bounded recall with an empty query. It does not reconstruct the transcript or reuse cached bodies. |
| `Stop` | Captures `last_assistant_message` as a `model_inference` candidate and flushes. Returns no continuation request. When `stop_hook_active=true`, it skips the continued Stop event. |

Captures are **pending proposals**. A user prompt is not automatically an
explicit fact, and an assistant completion is not treated as evidence of truth.
An authorized reviewer must inspect and accept or reject candidates separately.
See [ingestion](ingestion.md) for the review protocol and keyed-fact revision
checks. Capture truncates large events at 4,000 characters and labels the
truncation; these are bounded proposals, not transcript backups.

Event identity combines the host, project, fixed database binding, session,
agent, event name and `turn_id`. Retries of an identical event are idempotent;
changed text under the same event identity is a conflict. Identical text in
different turns remains distinct. Reinstalling does not change this identity.
The adapter uses an explicit agent ID when supplied, otherwise an opaque hash
of `transcript_path`; it never reads that transcript. Ambiguous subagent
identity is refused. It does not add a `SubagentStop` handler, so this is not a
claim of complete subagent capture.

Each returned packet contains the fresh record IDs and revisions. The runtime
stores only the context manifest as its read receipt, not recalled bodies.
Retrieval uses lexical mode, at most eight records and a 1,800-character core
budget. The adapter then removes whole records as needed, adjusting the
manifest to match, and bounds the **final UTF-8 stdout JSON** to 4,096 bytes by
default (`--max-output-bytes`, 1,024–16,384). Tiny budgets can reject a packet
when even its metadata will not fit. Error output never substitutes old bodies.

Generated context-producing handlers set `additionalContextLimit: 0` because
the adapter enforces its own serialized-byte cap; this avoids the host turning
a manifest into a spill-file preview. Their command timeout is
`3 × --timeout + 6` seconds (21 seconds by default); one IPC call defaults to
five seconds, one flush to at most eight requests and a five-second total
delivery budget.

Codex places `additionalContext` in developer context. The adapter labels its
JSON as untrusted retrieved data, but this is an advisory prompt boundary,
**not a separate low-trust model role**. A malicious stored body is still a
prompt-injection risk. The adapter cannot erase earlier prompts, ACP history,
compacted summaries, or native memory already loaded into a chat. Revalidate a
manifest or make a fresh read before consequential actions.

## Privacy, outages and changed database identity

Install-time `--memory-mode` chooses the behavior for this project adapter:

| Mode | Capture / outbox writes | Recall | Local read-receipt writes |
| --- | --- | --- | --- |
| `persistent` (default) | Yes, pending only | Yes | Metadata only |
| `incognito` | No | Yes | No |
| `temporary` | No | No | No |

Privacy is checked before capture, lock acquisition, event receipts and
outbox creation. The documented hook input does not establish the host's
current native privacy mode. Therefore this setting is an explicit adapter
policy; the adapter does not infer or automatically follow an App's private
chat toggle. Other hooks, native facilities and MCP servers have their own
policies.

If the daemon is offline, persistent capture can retain its bounded proposal
in the private outbox under the **original** binding. The hook exits zero with
a short unavailable message; it neither blocks the model nor uses cached
memory. Run this after service recovery:

```sh
recall-ledger-codex flush --project /absolute/project
```

A reused socket pointing to another namespace or history identity is refused
on every RPC. Restoring a database changes its history identity; the adapter
will not silently rebind or replay the old queue into it. Recover the intended
service or review the binding and preserved state before establishing a new
installation. A namespace label is an integrity boundary, not authentication
against another process running as the same OS user.

Known permanent candidate errors are retained in
`.codex/recall-ledger/dead-letter/` with the original request and machine code;
other candidates may continue. Transport, retryable and unknown-method errors
preserve the outbox. For manual recovery, first stop using the adapter, review
the error without changing the candidate payload or identity, fix its external
cause, and move the unchanged file back to `outbox/`; then run `flush`. The
`error` annotation is ignored on submission. Reusing the original identity
preserves conflict detection. The queue cap is 1,000 proposals. Event
fingerprints persist for replay protection and currently have no automatic
retention policy.

## Uninstall and recover

```sh
recall-ledger-codex uninstall --project /absolute/project
```

Uninstall verifies that generated configuration bytes have not changed,
exports the current readable records plus up to 100 pending metadata entries,
and restores the original hook file. By default it keeps both native memory
settings disabled. This avoids silently bringing older native memory back into
future chats.

Only after reviewing native memory and its intended scope, use:

```sh
recall-ledger-codex uninstall --project /absolute/project \
  --enable-native-after-review
```

That flag restores the original native setting bytes; it does not migrate
RecallLedger records into native memory. The fallback file is a snapshot
prepared during uninstall, may immediately become stale, and excludes pending
bodies. Keep the database and adapter state; use the core backup command for a
restorable backup.

Installation and uninstall use a persisted multi-file journal, not a filesystem
transaction covering all files at once. If interrupted, retry the same command
and options: it resumes only when files match the recorded old or intended
bytes. Unrelated edits produce `configuration_changed` and are never overwritten.
If the daemon is unavailable, uninstall stops before changing configuration so
the fallback can be prepared safely. Old loaded hook commands become inert once
the installation is marked uninstalled; already running model context is not
removed.

## Evidence and limits

Official protocol review: **E2**, checked on 2026-10-10:
[Codex hooks](https://learn.chatgpt.com/docs/hooks) and
[Codex memory](https://learn.chatgpt.com/docs/customization/memories).
The repository's synthetic tests exercise actual local Rust daemon processes
alongside fault/host-envelope fixtures. They cover pending admission, compaction
refresh, revisions, retraction, identity drift, privacy, bounded output and
configuration recovery. This is **local engineering validation**, not a paper
reproduction or E4 live Codex acceptance. No paid model or real user memory is
needed by these tests.

Run the targeted tests from the repository root:

```sh
RECALL_LEDGER_BIN="$PWD/target/debug/recall-ledger" \
PYTHONPATH=sdk/python:sdk/python/tests \
python3 -m unittest test_host_runtime test_codex test_kirocrew -v
```

Live acceptance still needs the installed Codex version, trusted/effective hook
readback, a warm follow-up and actual compaction, failure behavior, native
settings scope, and the particular root/subagent paths used by the project.
