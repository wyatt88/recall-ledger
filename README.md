# RecallLedger

Local memory infrastructure for agents, written in Rust. Its two product goals
are to reduce long-term memory distortion and the resulting task failures, and
to preserve useful knowledge when users move between agent brands.

RecallLedger stores versioned records and tracks the inputs used to derive new
records. When a source changes, its dependent results stop being readable. A
worker that finishes late must validate its inputs before it can publish.
Optional local models provide vector retrieval, automatic summaries, and repair
of managed summaries after their sources change.
Version 0.4 adds local ChatGPT/Codex hooks, a KiroCrew Gateway bridge, and an
OpenCode V1 plugin. Every adapter pins the intended namespace and database
history, so a reused socket cannot silently redirect queued memory.

**Development after v0.4.0:** portable namespace export, conflict preview,
transactional import, and version-checked task handoffs are implemented in this
source branch. [Run the A → B → A workflow](docs/migration.md). This code is not
included in the v0.4.0 tag.

**Source-preservation update, 2026-10-12:** automatic consolidation now defaults
to complete original-source evidence capsules, preserving structured scope and
conditions as well as text, with no model required. Revision
changes withdraw them before deterministic repair; insufficient budgets refuse
publication instead of silently dropping qualifications. Free-form summaries
remain explicit, unchecked interpretations. [Use and audit the strict path](docs/fidelity.md).

[中文说明](README.zh-CN.md) · [Architecture](docs/architecture.md) ·
[API contract](docs/CONTRACT.md) · [Operations](docs/OPERATIONS.md) · [Research](docs/research.md) ·
[Product goals and acceptance](docs/product-goals.md) ·
[Benchmark design](docs/benchmark/README.md) · [Test environments](docs/benchmark/ENVIRONMENT.md) ·
[Migration and handoff](docs/migration.md) · [Source preservation](docs/fidelity.md) · [Long-term fidelity evidence](docs/long-term-fidelity.md) ·
[Native memory integration](docs/native-memory.md) · [Roadmap](docs/roadmap.md) ·
[Claude Code setup](docs/claude-code.md) · [Admission](docs/ingestion.md) · [Recovery](docs/recovery.md) ·
[Local ChatGPT](docs/chatgpt.md) · [Codex](docs/codex.md) ·
[KiroCrew](docs/kirocrew.md) · [OpenCode](docs/opencode.md) · [Contributing](CONTRIBUTING.md)

## Why this exists

Long-running agents can lose important conditions during compression, repeat
corrected mistakes, or treat an old inference as a current fact. Changing agent
brands can then leave preferences, project decisions and useful experience behind.
RecallLedger aims to make that knowledge a durable, user-controlled local resource
that supported hosts can use.

These goals have separate acceptance criteria:

| Goal | What must be demonstrated |
| --- | --- |
| Reliable long-term memory | Evidence and its conditions survive repeated use and compression; corrections stop recurring in later tasks; useful task performance is preserved as history grows |
| Continuity across agent brands | A destination agent can use prior knowledge, continue a task with a fresh check of its environment, and return updates without losing scope or reviving withdrawn information |

Version and dependency checks address stale managed results. They do not by
themselves detect a faithful-looking but false summary. Sharing a ledger between
adapters also does not establish lossless native-memory import or task migration.
The [product goals](docs/product-goals.md) separate the existing mechanisms from
the implemented portable workflow and source-preservation checks from semantic checks, native-format mappings
and real-host evaluations still needed.

Suppose an agent remembers “the project language is undecided” and starts building
a summary. You then choose Rust. Updating the original record is not enough:
the old summary and the worker still processing the old input must be handled too.

RecallLedger makes this a storage contract:

```text
remember → prepare inputs at known revisions → process externally → publish
                 │                                                  │
                 └─ source changes → withdraw dependents ────────────┘
                                      reject stale publication
```

The lifecycle adapters capture pending proposals and read fresh memory before
each prompt. Explicit acceptance controls admission; conversation text does not
automatically become verified fact. Execution
checkpoints, model context, and model weights remain with their existing systems.

## What RecallLedger adds to native memory

**RecallLedger makes recorded memory dependencies part of the write, read, and
repair contract.** Its purpose is to keep a reported correction from leaving
obsolete managed summaries and vectors available for later use.

Native memory is a broad category. KiroCrew Memory V2 already specifies SQLite,
revisions, source-span receipts, atomic consolidation and privacy modes; it is
a strong comparison baseline, not merely a folder of Markdown files. Some hosts already have file search, semantic
retrieval, Git history, shared memory, and background consolidation. Markdown is
a useful editable format; SQLite, Rust, and MCP alone are not reasons to replace
an existing system.

The useful comparison is what happens when a remembered fact changes:

| Situation | RecallLedger 0.4 mechanism | Benefit within the managed path | Inspectable evidence |
| --- | --- | --- | --- |
| A corrected source has summaries derived from summaries | Revision-bound dependency edges and transitive invalidation in the mutation transaction | Descendants stop being served as current; updating only the original note is insufficient | [Core contracts](tests/core_contract.rs) |
| A slow worker finishes after the correction | `prepare`/`publish` input validation; managed workers also check leases and output revisions | Old work cannot overwrite a newer managed result or revive a withdrawn one | [Maintenance tests](tests/maintenance.rs) |
| A stale vector still scores highly | Embeddings bind to record revision and model key; recall enforces eligibility | Similarity cannot authorize an outdated record | [Vector contracts](tests/vector_contract.rs) |
| A write succeeds but its reply is lost | Mutation and idempotency receipt commit together; edits require `expected_revision` | Identical retries recover the result; competing edits receive a conflict | [Core contracts](tests/core_contract.rs) |
| A summary needs rebuilding after a restart | Persistent recipes, task generations, leases, bounded retries, stable output IDs | Managed repair resumes without making obsolete text readable while repair fails | [Runtime tests](tests/runtime_contract.rs) |
| Semantic indexing or the query model is unavailable | Explicit index coverage, character budgets, and reported lexical degradation | The caller can distinguish reduced retrieval from a complete semantic result | [API contract](docs/CONTRACT.md#recall) |
| A message corrects an existing fact | Pending candidates, exact scoped fact keys, revision-checked acceptance | Reviewed corrections retain the fact ID and trigger repair | [Admission tests](tests/ingestion.rs) |
| A packet or Markdown export predates a correction | Body-free change feed, context manifests, live-gated projection reads | Controlled readers detect stale revisions and time expiry | [Freshness](tests/changes.rs), [projection](tests/projection.rs) |
| A socket starts serving another namespace or database | Persisted `{namespace, history_id}` assertion checked inside each transaction, before reads, receipts and writes | Old queues and sessions refuse the new destination | [Binding](tests/binding.rs), [stdio MCP](tests/mcp_binding.rs) |
| A database needs recovery | SQLite online backup, new-path restore with a new history identity | Old tokens fail; old leases cannot publish | [Recovery tests](tests/recovery.rs) |

For example, changing “launch in April” to “launch in August” withdraws the
managed summary that used April, rejects any outstanding generation using that
revision, and queues a repair using the new input. The repaired summary retains
its ID with a new revision. This behavior was exercised in the
[local v0.2 model smoke](docs/VALIDATION-v0.2.md#real-local-model-smoke).
It does not prove that every generated summary is faithful or that an Agent
already holding April in its prompt will forget it.

These are implemented lifecycle properties, **not a demonstrated accuracy,
latency, or cost win over native memory**. A native system can implement the same
contracts. For small, stable workloads, files plus good search may be simpler.
The [roadmap](docs/roadmap.md) defines the comparisons required to establish a
meaningful advantage.

## What happens to MEMORY.md?

Adding MCP tools leaves the host's existing memory behavior in place. To make
RecallLedger authoritative, a host adapter must control memory capture, reads,
and refresh after corrections or context compaction.

| Integration level | Status | Ownership |
| --- | --- | --- |
| MCP / SDK alongside native memory | Available interfaces; host acceptance still required | Two stores coexist unless the application explicitly routes memory through RecallLedger |
| Managed Markdown compatibility | Implemented with explicit refresh and a live read gate | RecallLedger owns dynamic records; Markdown is a versioned, rebuildable view |
| Claude Code project hooks | Implemented; live host acceptance pending | Disable native auto memory in the selected project, capture pending proposals, inject fresh recall |
| Local ChatGPT / Codex hooks | Implemented for the documented local Codex runtime; App trust/loading still required | Share one project adapter; disable native memory generation/use in that scope and refresh after compaction |
| OpenCode V1 plugin | Implemented for v1.18.35; V2 unsupported | Fresh recall before model dispatch, pending capture; host sessions and compaction stay native |
| KiroCrew trusted Gateway bridge | Implemented callable bridge; upstream wiring required | The framework routes long-term reads and writes through RecallLedger; it keeps its own checkpoints |

Keep human-authored `AGENTS.md`, `CLAUDE.md`, and project rules. Learned facts
should enter a data channel, with provenance and revision metadata, rather than
automatically becoming instructions. Generated Markdown needs a manifest and a
freshness check; it must not become a second independently writable authority.
The [native memory design](docs/native-memory.md) explains migration, concurrent
edits, session refresh, and the limits of revoking copies.

## Current scope

The current source preview targets one machine:

| Available surface | Behavior |
| --- | --- |
| Rust library and CLI | SQLite records, revisions, explicit namespaces, validity windows |
| Derivation protocol | `prepare` captures inputs; `publish` validates all recorded dependencies |
| Revision and retraction | Transitive withdrawal of dependent records; guarded history and job reads |
| Retry handling | Durable idempotency receipts and receipt lookup |
| Recall | Literal text search, exact cosine vectors, hybrid RRF (`k=60`), character budgets |
| Local models | Explicit loopback embedding/chat endpoints; pinned model configurations |
| Automatic summaries | Durable recipes, bounded source groups, captured input revisions |
| Source preservation (post-v0.4) | Complete original-source capsules, guarded provenance, exact publication checks; default consolidation policy |
| Automatic repair | Withdraw stale outputs; leased workers rebuild managed summaries with stable IDs |
| Local daemon | Unix-domain socket on macOS/Linux; no HTTP listener |
| MCP adapter | Record operations, summary enqueue, and maintenance status over stdio |
| Python SDK | Client for the local daemon |
| Candidate admission | Explicit review, durable event deduplication, exact fact-key corrections |
| Freshness | Durable change cursor, recall manifest, current-context validation |
| Backup and restore | Consistent SQLite snapshot; new-path, new-history restoration |
| Markdown and host adapters | Managed projections, Claude Code migration/hooks, local ChatGPT/Codex hooks, Crew Gateway bridge, OpenCode V1 plugin |
| Experimental grouping | Deterministic grouping rules for later consolidation experiments |

The record store and lexical retrieval work without models. Semantic retrieval
and generated summaries require local model servers that you run and configure;
RecallLedger does not download weights or select a cloud provider. Exact cosine
search scans eligible vectors rather than using an ANN index.

Repository-state validation, learned memory selection, cost-optimal repair
selection, and benchmark quality results remain future work. Generated summaries
are not fact-checked, and model fluency is not evidence of source fidelity.

The next priorities are live host acceptance, applicability
checks against the current task/environment, and equal-budget behavioral
evaluation. See the [acceptance gates](docs/roadmap.md#delivery-gates).

## Build and try it

Prerequisites: Rust 1.89 or newer with Cargo and a C compiler for bundled
SQLite. The demo also uses Python 3. The daemon and demo target macOS/Linux.

From a checkout:

```sh
git clone https://github.com/wyatt88/recall-ledger.git
cd recall-ledger
cargo build --release --locked
./target/release/recall-ledger --help
bash examples/workflow.sh
```

Install the current source preview:

```sh
cargo install --git https://github.com/wyatt88/recall-ledger.git --branch main --locked
```

Migration and source-preserving repair require this development branch; the
`v0.4.0` tag predates them. Add `--force` when replacing an existing 0.4.0
installation, since the development package version has not been bumped.

The package has not been published to crates.io or PyPI. The Python SDK ships in
this repository. Releases distribute source; build for your local platform.
For reproducible installation, select a published tag or commit.

The demo uses a fresh temporary directory and synthetic records. It checks that
a summary prepared before a source revision cannot be published afterward, then
publishes a fresh summary and retracts its source.

For an interactive database:

```sh
mkdir -p .local-state
chmod 700 .local-state

./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  call remember --params \
  '{"body":"The project uses Rust.","kind":"commitment","idempotency_key":"decision-1"}'

./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  call recall --params '{"query":"Rust","max_chars":1200}'
```

Record and queue mutations need a nonempty `idempotency_key`. Reuse that key with
identical arguments when retrying an uncertain result. Reusing it with different
arguments returns a conflict. Use `operation` to retrieve the original receipt.
Maintenance passes manage their individual transaction keys internally.

## Run beside an agent

Choose the [local ChatGPT](docs/chatgpt.md), [Codex](docs/codex.md),
[KiroCrew](docs/kirocrew.md), or [OpenCode](docs/opencode.md) guide. Crew requires
trusted Gateway wiring; generated configuration alone does not complete host
acceptance. All of these paths run locally.

For Claude Code, start a daemon as below, then follow the
[project adapter installation guide](docs/claude-code.md). It preserves human
rules, captures pending proposals durably, and refreshes memory every prompt.
Uninstall exports current readable records and keeps native auto memory disabled
by default to avoid reviving old notes.

Edit [models.local.json](examples/models.local.json) to match your own local
servers: complete endpoint URLs, returned model names, explicit weight revisions,
and embedding dimensions. The example contains placeholders. No API key is used.
Then start one daemon for a database in a private local directory:

```sh
./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  --models examples/models.local.json \
  serve --socket .local-state/memory.sock
```

With an embedding provider, nonempty queries use hybrid retrieval by default;
without one, recall defaults to lexical search. `mode="vector"` requires semantic
retrieval to succeed. If a hybrid query's model call fails, the result explicitly
reports lexical degradation. Inspect `coverage` for missing vectors and fallback.

The worker indexes current records when configured, groups eligible unmanaged
roots sharing source/kind boundaries, and creates source-preserving capsules once
a bounded group contains at least four records. `--summary-policy model` opts
into model-written summaries. Source revision withdraws a managed result immediately,
then queues repair. The repaired record keeps its `output_id` and increments its
revision. Retraction cancels affected recipes. Manual `prepare`/`publish` outputs
are invalidated but are not automatically regenerated.

Omit `--models` for lexical retrieval and model-free evidence repair. Use `--no-worker` to disable background
maintenance, or `--auto-summary-min 0` to disable automatic grouping while retaining
explicit evidence/model recipes and configured indexing. See [operations](docs/OPERATIONS.md) for
budgets, retries, provider compatibility, and manual maintenance.

The namespace is selected by trusted transport configuration. Requests cannot
switch it by adding a namespace field to their parameters.

For an MCP-capable host, save its identity once after checking the intended
daemon, then configure the stdio command using absolute paths:

```sh
recall-ledger binding --socket /absolute/path/to/memory.sock > /absolute/path/to/binding.json
```

Keep that file; do not regenerate it on each startup. Without `--binding-file`,
MCP pins the daemon identity only for its current process lifetime.

```json
{
  "mcpServers": {
    "recall-ledger": {
      "command": "/absolute/path/to/recall-ledger",
      "args": ["mcp", "--socket", "/absolute/path/to/memory.sock",
               "--binding-file", "/absolute/path/to/binding.json"]
    }
  }
}
```

Start the daemon before the MCP adapter. The full derivation protocol is available
through the daemon, CLI, and Rust
library. Host-specific MCP configuration and approval behavior remain the host's
responsibility. The adapter targets MCP `2025-11-25` and provides tools only,
without sampling or task capabilities.

The Python SDK has no third-party runtime dependencies:

```sh
PYTHONPATH=sdk/python python3 examples/python_agent.py \
  --socket .local-state/memory.sock
```

```python
from recall_ledger import Client

memory = Client("/absolute/path/to/memory.sock")
receipt = memory.remember(
    "The project uses Rust.",
    kind="commitment",
    idempotency_key="decision-2",
)
result = memory.recall("Rust", max_chars=1200)
# Requires a configured summary provider; returns an enqueue receipt.
queued = memory.summarize(
    [receipt["id"]],
    max_chars=800,
    idempotency_key="summary-2",
)
tasks = memory.maintenance_status()
```

`summarize` returns a task and reserved output ID, not completed prose. Once the
task succeeds, use `inspect(queued["output_id"])`. The SDK's default timeout is
330 seconds for bounded local inference; it does not retry automatically.
If `TransportError.outcome_unknown` is
`true`, recover with `operation` and the original method/key, or retry identical
arguments with that key.

Use one authoritative long-term store. If a host independently copies a record
into its own profile or context, RecallLedger cannot retract that copy.

## Guarantees and limits

- Mutation and its idempotency receipt commit in the same SQLite transaction.
- A derived record is published only after its captured revisions, ancestry, and
  validity conditions have been checked in that transaction.
- Revising or retracting a source withdraws its transitive dependent results.
  Ordinary recall returns only currently readable records.
- Managed repair checks its lease, captured source versions, and output version
  before publication. A failed repair leaves the obsolete result unavailable.
- Retraction blocks subsequent managed reads and clears managed body content.
  It is **not secure erasure of SQLite pages, WAL files, backups, or copies already
  returned to another process**.
- A dependency means “this processor received this input,” not “this input proves
  every sentence true.” Hidden model context, unreported tool reads, and model
  training data are outside this contract.
- Namespaces are an application boundary, not a sandbox against hostile code
  running as the same operating-system user. CLI and library access are trusted.
- `export` is a diagnostic snapshot. Use `backup` and `restore` for recovery;
  restore creates a new history and invalidates old tokens. Changes after the
  backup are not retained. Manual file copying bypasses this protocol.
  Schema 1/2 databases migrate to schema 3 on open.
- Candidate origin labels and acceptance are supplied by trusted callers. Exact
  fact keys do not perform entity discovery or factual verification. Unknown
  copies remain outside retraction.
- The configured model `revision` is an operator declaration. The provider
  protocol does not prove which weights a server actually loaded. Change the
  revision whenever weights or encoding behavior change.
- Validity timestamps must be supplied correctly. The store cannot discover an
  unreported real-world change.

See [the architecture](docs/architecture.md) and [security policy](SECURITY.md)
for the boundaries in detail.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
PYTHONPATH=sdk/python RECALL_LEDGER_BIN="$PWD/target/debug/recall-ledger" \
  python3 -m unittest discover -s sdk/python/tests
```

CI runs the Rust checks on Ubuntu and macOS. A green test run establishes only
the cases covered by that run. It does not establish production readiness,
superior agent task performance, or successful paper reproduction.

[v0.4 validation](docs/VALIDATION-v0.4.md) and [review](docs/REVIEW-v0.4.md)
cover local host adapters and transaction-level binding.
[v0.3 validation](docs/VALIDATION-v0.3.md) retains the earlier lifecycle record. [v0.2 validation](docs/VALIDATION-v0.2.md) separates deterministic tests,
HTTP stubs, and the real local model smoke. The [review](docs/REVIEW-v0.2.md)
records independently designed counterexamples and remaining limitations.

## Research basis

The design draws on work about tiered context, typed memory, dependency repair,
state-aware reuse, and memory evaluation. [Research notes](docs/research.md)
identify six fixed paper versions, distinguish implemented mechanisms from
future work, and describe a testable grouping hypothesis.

The project is an original engineering implementation. It is not an official
implementation of those papers, and it does not claim their reported scores.

## License

Apache-2.0. See [LICENSE](LICENSE), [NOTICE](NOTICE), and
[third-party notices](THIRD_PARTY_NOTICES.md).
