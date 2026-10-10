# Integrating native memory and Markdown

Design review: 2026-10-10, updated for v0.4. The
[Claude Code project adapter](claude-code.md), [candidate admission](ingestion.md),
[managed Markdown and backup/restore](recovery.md), and
[change/context APIs](CONTRACT.md) now implement a bounded part of this design.
Version 0.4 also implements [local ChatGPT](chatgpt.md)/[Codex](codex.md) hooks,
[OpenCode V1](opencode.md) dispatch integration and a callable [KiroCrew](kirocrew.md)
Gateway bridge. The Crew bridge requires trusted upstream wiring. Fixture and
real-daemon tests do not establish live host acceptance.
Generic framework backends, automatic semantic extraction and arbitrary
two-way Markdown synchronization remain proposals.

## Ownership is the integration problem

An extra `recall` tool does not change a host that reads its own `MEMORY.md`,
updates another profile, and compacts old facts into a session summary. Changing
one file cannot remove those other copies. A dependable integration must specify:

1. **Write ownership:** which events become records, and which path commits them.
2. **Read ownership:** where a model request obtains its long-term facts.
3. **Refresh ownership:** when superseded facts leave the managed context.

One authoritative path can support multiple agents. They use revisions and
stable event idempotency keys to coordinate writes. It does not mean only one
agent may write.

## Separate three kinds of state

| State | Owner in the proposed integration |
| --- | --- |
| Human-authored rules, permissions, `AGENTS.md`, `CLAUDE.md` | The user and the host's instruction system |
| Learned facts, preferences, decisions, observations, managed summaries | RecallLedger, after an explicit per-scope cutover |
| Conversation transcripts, execution checkpoints, pending tool calls | The host/runtime that understands their execution semantics |

Keep retrieved facts in a reference-data channel. Copying untrusted learned text
into an instruction file does not preserve the current
[`retrieved_data` contract](CONTRACT.md#recall).
Rules can explain the memory workflow without embedding arbitrary memory bodies.

## Integration levels

| Level | What it can establish | What it cannot establish |
| --- | --- | --- |
| Tool coexistence | The Agent can read/write ledger records | Automatic capture, exclusive ownership, or native-cache refresh |
| Managed Markdown view | A controlled reader consumes generated files at a known generation | Control over other directories or already captured prompts |
| Native backend adapter | Long-term reads/writes follow the ledger contract | Replacement of checkpoints or revocation of model state |

The latter two require host-specific implementation and acceptance tests.
Filesystem watching or changing a memory directory alone is not a backend adapter.

## Proposed managed flow

```text
host events → admission / source mapping → RecallLedger
                                             ├─ per-request reference data
                                             └─ generated Markdown + manifest
user edits to a generated view → proposed patch → revision check → ledger
```

Start with one host, one scope, and one-way ledger-to-Markdown projection.
The host must stop independently writing that dynamic memory scope before the
projection is authoritative. Human edits can initially be refused with guidance
to the record interface; a future edit importer must turn them into explicit
patches against the original generation. Silent last-writer-wins merging is not
acceptable.

Per-task retrieval belongs in a session-specific context package. Two agents
must not overwrite the same shared `MEMORY.md` with different task-specific
results. A shared project snapshot and a per-session retrieval result have
different lifetimes.

### Projection contract

The v0.3 projection manifest identifies database history, commit/generation, record
IDs and revisions, and content hashes. Generate into temporary files, then
publish a complete generation atomically through a manifest/pointer switch.
Readers must pin and validate that generation. Several unrelated file renames
do not constitute an atomic directory publication.

The database commit and filesystem publication are separate operations. If
projection fails after a commit, the read gate detects a stale generation and
regenerates or reports unavailable memory. It must not silently accept the old
file as current. The projection is rebuildable; its absence is not record loss.

Retraction removes the body from the next accepted managed generation. Stable
source mappings and withdrawal metadata must prevent an old file import from
resurrecting that record. A missing line is not automatically a retraction:
truncation, reformatting, and budget selection can also remove lines.

### Context refresh contract

Load a replaceable reference block before the relevant model request. Refresh it
after revision, retraction, session restoration, or compaction. Track which
records and revisions each package used.

If a host only reads memory at startup, promise freshness for a **new session**,
not every turn. If old facts persist in the transcript or compaction result, a
fresh reference block alone cannot prove that the model will ignore them.
State that limitation in the host's compatibility record.

Checking dependencies immediately before a consequential action can reduce
staleness, but it is not an atomic transaction with the external action.
Execution authorization remains a separate concern.

## Official host controls inspected

The following is documentation review as of 2026-10-10, not a record of changing
any user's settings or testing those hosts.

| Host | Documented control | Consequence for an adapter |
| --- | --- | --- |
| Claude Code | `autoMemoryEnabled: false` or `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`; `autoMemoryDirectory` changes the Markdown directory | Disabling auto memory requires a separate adapter read path. Merely exporting to its former directory does not restore automatic loading. |
| Codex | `memories.generate_memories` controls newly created threads' contribution; `memories.use_memories` controls injection into future sessions | Plan cutover for new sessions, account for existing processes/jobs, and use host integration for ledger reads. Do not treat generated memory files as the supported editing surface. |

These pages did not document a switch to an arbitrary MCP or SQLite memory
backend. Controls can change; pin and test the actual host version before
shipping an adapter.

Sources and reading scope:

- [Claude Code memory](https://code.claude.com/docs/en/memory): instruction files,
  auto memory, directory layout, loading, and disable controls.
- [Codex memories](https://learn.chatgpt.com/docs/customization/memories):
  generation, session use, generated-state guidance, and controls.
- [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference):
  `memories.generate_memories` and `memories.use_memories`.

These switches do not erase files or remove text from a running conversation.
In particular, a setting affecting newly created threads is not proof that old
queued processing has stopped.

## Migration and rollback

Version 0.3 imports an explicitly selected Markdown directory as pending
candidates with path/hash/chunk provenance. Originals are preserved. Uninstall
exports current readable records and keeps native auto memory disabled by
default. This bounded implementation does not complete every acceptance item
below, especially live-host shadow comparison and compaction behavior:

1. Inventory only the selected dynamic memory scope; classify human rules
   separately. Preserve originals and path/content hashes.
2. Import with stable source mappings. Preserve raw evidence separately from
   extracted interpretations; do not silently promote ambiguous text to facts.
3. Run shadow reads and compare omissions, conflicts, and permissions before
   assigning ownership.
4. Quiesce native writers for the chosen scope; drain or account for outstanding
   work. Enable the managed read/write/refresh path in a new session.
5. Exercise corrections, concurrent writes, retractions, crashes during
   projection, compaction, and cross-agent reads.
6. On rollback, preserve post-cutover changes and withdrawals. Restoring an old
   Markdown backup alone would lose new information or resurrect deleted facts.

The current `export` operation is a diagnostic snapshot. Full database backup
and point-in-time restoration use the separate [recovery API](recovery.md);
neither operation converts arbitrary notes into verified structured facts.

## Boundary that no adapter should hide

Ledger withdrawal is enforced for subsequent managed reads. It does not securely
erase SQLite pages, Git history, backups, third-party profiles, or text already
sent to a model. A host that cannot expose or control its memory read/write
paths can support coexistence, but should not be advertised as fully managed.

See the [roadmap and acceptance gates](roadmap.md) for how this design is intended
to become a tested integration.
