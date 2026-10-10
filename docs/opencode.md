# OpenCode V1 local memory plugin

The adapter adds automatic fresh reads and candidate capture to OpenCode
**v1.18.35**. It does not replace OpenCode's session database, compaction,
checkpoints, instruction files or the model's weights. Captured text is always a
pending proposal; a separate explicit admission action is required before it
becomes recallable memory.

This entrypoint does **not** support OpenCode V2. V2 has a different package and
plugin lifecycle. The official migration guide explicitly says that V1 plugin
implementations do not run in V2.

## Install and remove

Requirements: a Unix-like host, Python 3.9+, the RecallLedger Python SDK,
a daemon supporting `binding` and transaction-level `expected_binding`,
and OpenCode V1. JavaScript uses only Node-compatible standard-library APIs
available in OpenCode's Bun runtime; it has no npm dependency. The Python wheel
contains the complete runtime, so normal installation does not require a
repository checkout or npm download.

Start the daemon for the desired namespace, then install from the repository:

```sh
recall-ledger --db /absolute/path/to/memory.db --namespace my-project \
  serve --socket /absolute/path/to/memory.sock --no-worker
```

In another terminal, after installing the Python distribution:

```sh
recall-ledger-opencode install \
  --project /absolute/path/to/my-project \
  --socket /absolute/path/to/memory.sock
```

For development from the RecallLedger checkout, the equivalent invocation is:

```sh
PYTHONPATH=sdk/python python3 -m recall_ledger.opencode install \
  --project /absolute/path/to/my-project \
  --socket /absolute/path/to/memory.sock
```

The project directory must exist. Installation verifies the daemon and records
`{namespace, history_id}`. A later socket reassignment or database-history change
fails with `binding_mismatch`; neither retries nor reinstall silently accept a
new binding. The server checks the expected identity on every IPC operation,
including writes, rather than relying only on a preliminary status read.

Installation adds:

```text
.opencode/
  plugins/recall-ledger.js       # one automatically discovered plugin export
  recall-ledger/
    config.json                 # pinned binding, Python runtime and enabled switch
    runtime.mjs                 # installed JavaScript implementation
    .gitignore                  # exclude this entire private state directory
    sessions/                   # per-session commit token and revision manifest
    outbox/                     # pending captures awaiting acknowledgment
    receipts/                   # event fingerprint and candidate ID, no body
```

It does not edit `opencode.json`, JSONC files, `AGENTS.md`, other plugins or
global settings. The state directory is mode 0700 and files written by the
installer/bridge are mode 0600. Pending outbox files contain the captured text;
they are not encrypted. Successful delivery removes that queued text and stores
only a receipt. Neither this nor database retraction promises physical erasure
from backups, filesystem journals, swap or host transcripts.

Restart OpenCode after installation. Keep the Python environment and SDK path
available. The canonical implementation is bundled at
`recall_ledger/resources/opencode_runtime.mjs`. The optional `--plugin-source`
override accepts a complete self-contained runtime for controlled development;
the repository's `integrations/opencode/runtime.mjs` is only a development
re-export and must not be used as that override.

Remove it with:

```sh
PYTHONPATH=sdk/python python3 -m recall_ledger.opencode uninstall \
  --project /absolute/path/to/my-project
```

Uninstall first disables the managed configuration, then removes its unchanged
plugin entry. State remains available for inspection or reinstall with the same
binding. It does not restore an old Markdown snapshot into active memory. If a
user edited the entry, removal stops instead of deleting those changes.
An already-running hook can finish an in-flight operation; restart the host to
complete unloading.

Installation records a prepared receipt before creating its files, so repeating
installation can recover interruption. The installer does not overwrite
pre-existing unowned files or silently discard edits. This is a project-local
plugin: moving the project, Python environment or SDK requires an explicit
reinstall at the intended location. Install, uninstall and bridge mutations
share one project lock. Concurrent installation attempts re-read the winning
binding under that lock and cannot silently replace it.

## Hook behavior

| Hook / event | Actual behavior |
| --- | --- |
| `chat.message` | Submits visible user text with the durable message ID as `origin=user_message`. Synthetic/ignored parts, reasoning and non-text parts are excluded. The hook runs before durable host admission; a later host failure does not promote the proposal to a fact. |
| `experimental.chat.system.transform` | Before each agent model request with a session ID, reads the current user query through the host SDK and calls `recall`. Uses lexical mode to avoid invoking embedding providers. Adds one bounded, explicitly untrusted memory packet. No cached body fallback. |
| `session.status` with idle / `session.idle` | Reads at most 32 recent messages via the SDK. Captures at most eight completed visible assistant messages belonging to the latest user turn as `origin=model_inference`. Excludes errors, summaries and tool-call steps. Duplicate callbacks retain the same event and idempotency key. |
| `experimental.session.compacting` | Adds a reminder to refresh external memory after compaction; never writes a memory-body snapshot or replaces the host compaction prompt. The next agent model request performs a fresh read. |
| `dispose` | Stops accepting new work and drains already-started per-session work. |

The plugin checks each session's SDK-reported directory against the installed
project. Unknown sessions, sessions from another directory and child sessions
with `parentID` are excluded. It supports concurrent top-level sessions through
independent queues and independent persisted manifests. The daemon namespace
is still fixed by its deployment; different projects requiring isolation need
separate daemon bindings.

No private/incognito field is invented or inferred. To stop this adapter from
reading or capturing new content, set its managed `config.json` `enabled` value
to `false`, or uninstall it. The plugin reads that switch before accessing
session content. This controls this adapter, not OpenCode's own persistence.
Already-started callbacks may finish.

## Consistency, retry and limits

- Session manifests contain record IDs/revisions and commit tokens, never
  retrieved bodies. `minimum_commit` carries the previous session read barrier.
- All operations assert the installed binding. Namespace/history mismatch stops
  delivery and injection; old outbox content cannot migrate to a new database
  just because it appeared on the same socket. Passing a real SDK `Client`
  directly to `bridge` also requires matching `expected_binding`; unpinned or
  mismatched clients are rejected before IPC or queue changes.
- Capture identity uses project, host session ID, message ID and role. Repeated
  content with different message IDs remains distinct. Changing the body under
  an existing ID is rejected instead of silently replacing a proposal.
- Capture while the daemon is unavailable and writes with a lost response remain
  queued under the installed binding with identical arguments and key. A known
  binding mismatch refuses new capture as well as delivery.
  Each bridge call retries at most eight queued events. Terminal event conflicts
  retain only a quarantine fingerprint; transport or binding failures retain the
  pending queue.
- Messages above 12,000 characters are skipped rather than silently truncated
  into a different proposition. The pending queue holds at most 1,000 messages.
  SDK history is bounded to 32 messages, so extremely long tool-heavy turns may
  not expose the parent user message and are skipped. Capture failure is logged
  as a generic diagnostic without copying bodies into logs.
- Each IPC call defaults to a two-second timeout. The child bridge has a bounded
  lifetime. Adapter filesystem mutations are serialized using a local advisory
  lock; session ordering is independently preserved in JavaScript.
- If recall, host SDK reads or binding checks fail, the plugin emits an
  unavailability notice and continues the host turn without cached memory.
  This is availability-oriented behavior, not a promise that the model has
  forgotten text already present in its conversation.

A current `retract` prevents future RecallLedger reads from returning that
record. It cannot retroactively remove host output, prior model-visible packets,
copies inside a host summary or text copied elsewhere. The compaction reminder
is advisory; it does not prove summary contents are free of stale facts.
Pending model proposals are deliberately not auto-promoted into facts or
automatically merged by semantic similarity.

Inspect delivery state without invoking a model:

```sh
printf '%s\n' '{"action":"status"}' |
  PYTHONPATH=sdk/python python3 -m recall_ledger.opencode bridge \
    --config /absolute/path/to/my-project/.opencode/recall-ledger/config.json
```

Explicit queue flush uses `{"action":"flush"}` on the same command. A restored
database with a new history ID requires an explicit migration decision; this
adapter intentionally has no “accept whatever appears on this socket” recovery.

## MCP is separate

OpenCode V1 also supports local MCP through:

```json
{
  "mcp": {
    "recall-ledger": {
      "type": "local",
      "command": [
        "/absolute/path/to/recall-ledger",
        "mcp",
        "--socket",
        "/absolute/path/to/memory.sock",
        "--binding-file",
        "/absolute/path/to/binding.json"
      ],
      "enabled": true
    }
  }
}
```

Create `binding.json` by explicitly reading the intended daemon's `binding`
command and retaining only its `{namespace, history_id}` object. The automatic
plugin installer does not enable MCP or grant model tools. MCP can be added
separately for deliberate inspection/admission workflows; its existence does
not guarantee automatic recall or capture. Those behaviors are provided by the
plugin above. Model tool permission and human approval remain host policy.

## Official source pin and verification

Reviewed on **2026-10-10**:

- [OpenCode plugin documentation](https://docs.opencode.ai/docs/plugins/):
  local JavaScript discovery, plugin export contract, event names and compaction.
- [MCP documentation](https://docs.opencode.ai/docs/mcp-servers/):
  V1 `mcp` configuration, local command arrays and tool availability.
- [V2 migration guide](https://opencode.ai/v2/docs/build/plugins/migrate-v1):
  V1 implementations are incompatible; V2 uses session `prompt`, `context` and
  `compaction` hooks.
- Official [v1.18.35](https://github.com/anomalyco/opencode/releases/tag/v1.18.35),
  commit `53d1eabb61e21162157817bf677da0a4ad3332e3`.
- Fixed-source [plugin hook types](https://github.com/anomalyco/opencode/blob/53d1eabb61e21162157817bf677da0a4ad3332e3/packages/plugin/src/index.ts),
  [prompt admission](https://github.com/anomalyco/opencode/blob/53d1eabb61e21162157817bf677da0a4ad3332e3/packages/opencode/src/session/prompt.ts),
  [model request assembly](https://github.com/anomalyco/opencode/blob/53d1eabb61e21162157817bf677da0a4ad3332e3/packages/opencode/src/session/llm/request.ts),
  [compaction](https://github.com/anomalyco/opencode/blob/53d1eabb61e21162157817bf677da0a4ad3332e3/packages/opencode/src/session/compaction.ts),
  [SDK request methods](https://github.com/anomalyco/opencode/blob/53d1eabb61e21162157817bf677da0a4ad3332e3/packages/sdk/js/src/gen/sdk.gen.ts),
  and [SDK message/session types](https://github.com/anomalyco/opencode/blob/53d1eabb61e21162157817bf677da0a4ad3332e3/packages/sdk/js/src/gen/types.gen.ts).

Reading scope: hook declarations, their prompt/compaction invocation sites,
message/session schemas and SDK get/messages requests. This is source/API review
(E2), not a full security audit of OpenCode.

Run isolated tests from this checkout:

```sh
node --test integrations/opencode/tests/plugin.test.mjs
PYTHONPATH=sdk/python RECALL_LEDGER_BIN="$PWD/target/debug/recall-ledger" \
  python3 -m unittest discover -s sdk/python/tests -p test_opencode.py -v
```

To include the isolated distribution test, build a wheel and set
`RECALL_LEDGER_WHEEL` to its absolute path alongside `RECALL_LEDGER_BIN`.
The test creates a fresh virtual environment, installs that wheel with
`--no-index --no-deps`, invokes its installed command from outside the checkout,
and exercises the installed JavaScript/Python/daemon chain and uninstall.

The JavaScript tests execute the plugin hooks against SDK-shaped fixtures. The
daemon tests load the actual installed JavaScript entry and run the real Python
bridge and Rust daemon, covering pending admission, repeat delivery, revisions,
retraction, offline queue recovery and same-socket namespace rebinding.
On 2026-10-10, all eight JavaScript fixtures and all fifteen Python tests
(including five real-daemon integration tests and isolated wheel installation)
passed with Node 26.10.0 and
Python 3.14.8 against the local v0.4 daemon build. This is a local integration
experiment, not an OpenCode end-to-end acceptance result.

The tests do not launch a paid model or a full OpenCode process.
Real-host loading, SDK event scheduling, Bun process
behavior, long-session compaction and desktop UI acceptance remain separate
validation steps. No user host configuration is changed by the test suite.
