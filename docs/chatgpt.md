# Local ChatGPT Agent App integration

This guide targets the **local ChatGPT Agent App**, using its local Codex
runtime. RecallLedger runs beside that runtime, with a Unix socket for memory
operations and local project hooks for automatic capture and retrieval.

The entrypoint requires a local App surface that exposes the documented Codex
project hooks and memory configuration. A desktop window alone does not prove
that those interfaces are enabled for a particular chat. Check effective hooks
and settings in that App before treating installation as active integration.

## Install

Use macOS/Linux and Python 3.11+. From the RecallLedger checkout, install the
Python SDK into the Python environment you will keep available:

```sh
python3 -m pip install ./sdk/python
```

Start the local daemon for the intended project:

```sh
recall-ledger --db /absolute/private/memory.db --namespace my-project \
  serve --socket /absolute/private/memory.sock --no-worker
```

In another terminal:

```sh
recall-ledger-chatgpt install \
  --project /absolute/project \
  --socket /absolute/private/memory.sock
```

`recall-ledger-chatgpt` and `recall-ledger-codex` share the same implementation
and project state. Installing both for the same project does not duplicate
hooks. The installer preserves existing hook groups, adds user-turn,
post-compaction session-start and completion handlers, and disables native
memory generation/use in the selected project's configuration scope.
Human-authored rules and global memory files remain under their existing owners.

Review and trust the exact generated hooks using the App's hook interface
(`/hooks` where available), then start a new session. Inspect effective hooks
and memory settings, including any inherited or managed configuration. The
installer returns `host_trusted: false`, `effective_hooks_verified: false`, and
`live_session_verified: false`; it cannot grant or verify these host states.

## Automatic lifecycle and optional tools

The hooks capture user/assistant text as **pending candidates**, read fresh
memory before each user prompt and after compaction, and preserve offline
captures under the original database binding. An explicit review/admission step
is required before a candidate becomes recallable. Native session checkpoints,
conversation history and compaction are still owned by the App.

Automatic hooks do not require the model to choose a memory tool. For explicit
inspection or candidate review, separately register this local stdio MCP command
in the App's supported MCP configuration surface:

```text
/absolute/path/to/recall-ledger mcp \
  --socket /absolute/private/memory.sock \
  --binding-file /absolute/project/.codex/recall-ledger/binding.json
```

This is an executable plus arguments, not a URL. Use the host's configuration
format and tool approval settings. The MCP server exposes the normal memory
tools; it does not inherit the hook adapter's private-session policy.

The two CLIs also share maintenance commands:

```sh
recall-ledger-chatgpt flush --project /absolute/project
recall-ledger-chatgpt uninstall --project /absolute/project
```

Uninstall retains native memory as disabled by default until its existing
content has been reviewed. See the [shared Codex guide](codex.md) for restoration,
configuration recovery, pending-candidate review and output budgets.

## Privacy and acceptance limits

Choose `--memory-mode incognito` at installation for reads without capture or
local read-receipt writes, or `--memory-mode temporary` for neither reads nor
writes. This is an explicit adapter policy. Hook input does not prove the App's
native private-chat mode; the adapter does not automatically follow that toggle.
Do not expose independent MCP write tools to such sessions without their own
trusted routing policy.

The hook packet is labeled as retrieved data, but the host inserts it into
developer context. This label does not provide formal prompt-injection isolation.
RecallLedger can stop future managed reads of stale records; it cannot remove
text already in a chat or another application's memory.

The local alias, shared hooks and actual daemon paths are covered by synthetic
engineering tests. Full acceptance in a running ChatGPT Agent App, effective
trust/settings, warm turns and real context compaction remain unverified.
Official interface review is E2; this is not an E4 deployment claim.

Sources checked on 2026-10-10:
[local hooks](https://learn.chatgpt.com/docs/hooks),
[memory settings](https://learn.chatgpt.com/docs/customization/memories),
[MCP](https://learn.chatgpt.com/docs/extend/mcp).
