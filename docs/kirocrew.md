# KiroCrew trusted Gateway bridge

RecallLedger provides a callable Python bridge and subprocess CLI for trusted
KiroCrew Gateway code. It exposes `capture`, `read` and `refresh` with frozen
member/store/session/privacy routing. It does **not** install an App that
transparently replaces KiroCrew Memory V2.

KiroCrew already has native learned-state storage, revisions, provenance,
retrieval, consolidation and privacy rules. This bridge is an additional
cross-host ledger. Keep an explicit authority boundary for shared facts; two
sequential writes to native V2 and RecallLedger are not an atomic transaction.

## Provision one binding

Start a dedicated local daemon for the member/store's intended namespace.
Install the SDK, then generate bridge state in an explicit private directory:

```sh
python3 -m pip install ./sdk/python
recall-ledger --db /absolute/private/member.db --namespace member-123 \
  serve --socket /absolute/private/member.sock --no-worker

recall-ledger-kirocrew install \
  --state /absolute/private/crew-member-ledger \
  --socket /absolute/private/member.sock \
  --host-instance-id gateway-1 \
  --member-id member-123 \
  --store-id store-456 \
  --binary /absolute/path/to/recall-ledger
```

The parent state directory must exist. Python 3.9+ and Unix file/socket support
are required. This writes only that state directory: a private runtime config,
the verified daemon `namespace/history_id`, a recovery journal, and
`mcp.example.json`. It does not modify provider global configuration, KiroCrew's
generated agent files, native V2 databases, or Markdown memory.

`mcp.example.json` is an example for KiroCrew's owned MCP configuration surface.
It runs the core MCP server with the fixed `--binding-file`. Installation
reports `gateway_wired: false`. Registering this server can make tools
available; it does not wire lifecycle callbacks. The full MCP tool surface is
not privacy-filtered by this bridge: do not expose it to private sessions
without a separate trusted tool-routing policy. Candidate admission rules
below apply to **bridge capture**, not to arbitrary other write tools.

## Wire trusted Gateway code

Build `FrozenContext` from KiroCrew's frozen `ExecutionContext` and the
Gateway's authenticated store/session selection. Never let model arguments,
display names, message text or project paths select these values.

```python
from recall_ledger.host_runtime import Runtime, context_text
from recall_ledger.kirocrew import FrozenContext, GatewayBridge

# These values come from trusted Gateway routing, before model execution.
context = FrozenContext(
    host_instance_id="gateway-1",
    member_id="member-123",
    store_id="store-456",
    memory_mode="persistent",  # persistent, incognito, temporary
    session_key="native-session-id",
    agent_id="root",
)
bridge = GatewayBridge(
    Runtime.load("/absolute/private/crew-member-ledger"), context
)
bridge.capture(
    event_id="user:stable-source-span-id",
    body="Already admitted source text",
    origin="user_message",
)
bridge.flush()
packet = bridge.refresh(query="The current task")
if packet["available"]:
    retrieved_data_context = context_text(packet)
    # Pass this as retrieved data through your Gateway's context assembly.
```

These are implemented RecallLedger methods, not claims about current upstream
KiroCrew method names. An executable integration helper is provided at
[`integrations/kirocrew/gateway_bridge.py`](../integrations/kirocrew/gateway_bridge.py).
It is a Gateway-callable helper; importing it does not register any App or hook.

Call `refresh` before every model dispatch, including warm follow-ups, resumed
work and post-compaction continuations. Pass the previous manifest when the
caller retains one. Validation is informative; the bridge always makes a new
read, including when no change event was observed, because expiry can occur
without a write. Successful refresh returns a fresh manifest and no cached
bodies. On an error, the Gateway must discard the external context for that
dispatch or stop the dispatch according to its own policy; it must not
silently reuse the previous packet.

Capture only sources admitted by the Gateway. `event_id` must identify a
stable native event/source span across retries, not a random ID generated per
delivery. Qualify IDs by source event kind. Assistant text should use
`origin="model_inference"`; tool observations use `tool_observation`.
Every capture is pending until explicit review. Inspect/accept/reject via the
[ingestion protocol](ingestion.md); no bridge method auto-accepts candidates.
Large proposals are truncated at 4,000 characters and marked, so they are not
a complete source archive.

## Subprocess interface

A Gateway that needs a process boundary can write a private, owner-only JSON
context file with exactly these fields:

```json
{
  "host_instance_id": "gateway-1",
  "member_id": "member-123",
  "store_id": "store-456",
  "memory_mode": "incognito",
  "session_key": "native-session-id",
  "agent_id": "root"
}
```

The Gateway chooses the file path and freezes its contents for this execution;
do not expose that path as a model-controlled tool argument. The CLI refuses
symlink paths, files owned by another user and group/world permissions.
File ownership is not authentication against other code running as the same
OS user; trusted Gateway routing remains required.

```sh
printf '%s\n' '{"operation":"read","query":"The current task"}' |
  recall-ledger-kirocrew call \
    --state /absolute/private/crew-member-ledger \
    --context-file /absolute/private/execution-context.json
```

The stdin envelope accepts only operation-specific fields:

| Operation | Fields |
| --- | --- |
| `capture` | `operation`, `event_id`, `body`, optional `origin` |
| `read` | `operation`, optional `query` |
| `refresh` | `operation`, optional `query`, optional `manifest` |
| `flush` | `operation` |

Member IDs, privacy mode, socket and store paths in stdin are rejected.
Successful calls return bounded JSON on stdout. Errors return exit 1 and a
body-free code on stderr. This CLI is **not a direct KiroCrew ScriptHookStore
Stop script**: do not interpret its acknowledgement as hook context or
`decision:block`, which can request another model turn.

## Enforced boundaries

- Persistent mode permits pending capture and fresh reads. Incognito permits
  reads but no learned writes, outbox delivery or local read-receipt writes.
  Temporary performs no memory reads or writes. The gate runs before capture
  and enqueue. The mode must come from the trusted frozen context.
- A bridge's host/member/store must match the installation. Every actual RPC
  pins the daemon namespace/history identity. Swapping a socket, restoring a
  different history or selecting another member fails; no Global fallback or
  automatic rebind exists.
- Event identity includes the host, member/store binding, fixed database
  identity, session, agent and native event. Retries are stable across reinstall.
  Different agents cannot merge under a shared parent session ID.
- Capture and flush are separate. Offline capture can persist a proposal under
  its original binding; delivery never changes that binding. Flush is bounded
  to eight requests per batch and a five-second delivery budget. The outbox
  holds at most 1,000 candidates.
- Permanent candidate errors remain in `dead-letter/` with original request
  and machine code. Unknown-method, transport and retryable failures retain
  the outbox. Review and repair the cause before manually moving an unchanged
  dead-letter file back to `outbox/` and calling `flush`; do not change its
  event identity or payload to suppress a conflict.
- Read output defaults to a 4,096-byte final JSON cap. Entire records are
  omitted with matching manifest changes when necessary. Context is untrusted
  data, and wrappers do not neutralize malicious instructions. Retraction
  prevents subsequent reads; it does not erase native history or text already
  sent to a model.

State includes proposal bodies, fingerprints, metadata receipts and exports.
Keep it outside published repositories and preserve it with the canonical
database. There is no automatic receipt-retention policy in this release.

## Uninstall and recovery

```sh
recall-ledger-kirocrew uninstall \
  --state /absolute/private/crew-member-ledger
```

This checks known generated files, prepares a current readable-record export
plus at most 100 pending metadata entries, and reverses generated-file changes.
It leaves native V2 untouched and retains bridge state/outbox. Stop calling
the bridge from Gateway code and remove any separately registered MCP entry
as a distinct configuration change.

The export is a prepared-uninstall snapshot, not a restorable database backup
or native V2 migration. Preserve the database; pending proposal bodies remain
there or in the outbox. A journal resumes interrupted install/uninstall only
when file bytes match recorded states, and refuses unrelated edits. If the
daemon cannot provide a binding-checked snapshot, uninstall stops before
changing configuration.

## Evidence and upstream seams

Official implementation review: **E2**, pinned to KiroCrew commit
[`950be416840623da0cda74be10e3acfee94e434f`](https://github.com/kirodotdev/KiroCrew/commit/950be416840623da0cda74be10e3acfee94e434f),
checked on 2026-10-10:

- [Frozen ExecutionContext](https://github.com/kirodotdev/KiroCrew/blob/950be416840623da0cda74be10e3acfee94e434f/src/kiro_crew/execution_context.py#L238-L322)
  and [native memory/privacy contract](https://github.com/kirodotdev/KiroCrew/blob/950be416840623da0cda74be10e3acfee94e434f/docs/system-specs/modules/memory-skills-hooks.md#L5-L110).
- [Context assembly](https://github.com/kirodotdev/KiroCrew/blob/950be416840623da0cda74be10e3acfee94e434f/src/kiro_crew/context.py#L3119-L3227)
  and [source/privacy admission](https://github.com/kirodotdev/KiroCrew/blob/950be416840623da0cda74be10e3acfee94e434f/src/kiro_crew/history_consolidation.py#L1332-L1410).
- [App boundary and gaps](https://github.com/kirodotdev/KiroCrew/blob/950be416840623da0cda74be10e3acfee94e434f/docs/architecture/overview.md#L501-L560):
  App startup/shutdown/routes are not a complete core memory interception seam.
- [MCP configuration merge](https://github.com/kirodotdev/KiroCrew/blob/950be416840623da0cda74be10e3acfee94e434f/docs/architecture/mcp.md#L1-L145).

Synthetic tests use the actual RecallLedger daemon and trusted-context fixtures
to check candidate admission, privacy, wrong bindings, refresh and retraction.
These are local engineering checks, **not a live KiroCrew acceptance or an
upstream patch**. Dashboard/Slack/task runner lifecycles, source-span
consolidation transactions, process restart and the exact installed upstream
version still require host integration and E4 acceptance.
