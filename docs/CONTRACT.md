# v0.4 contract

This document describes the source preview. It does not establish production
readiness, model grounding, or benchmark quality.
The [v0.1 contract](IMPLEMENTATION-CONTRACT.md) is retained as a historical record.
The current source branch adds the portable extension below after v0.4.0 and
upgrades database schema 3 to 4 without changing the existing history identity.
Use a backup before upgrading; older binaries cannot open schema 4.

## Source-preservation extension (development)

`consolidate` is the runtime alias for deterministic `evidence_create`.
`evidence_sources` previews complete original sources and the recorded closure;
`evidence_inspect` audits a local managed capsule against that current closure.
Source objects retain `fact_key` as the full entity/attribute/scope/conditions
object, or `null` when unbound. Ordinary inspect, recall and captured job reads
preserve it too. Recall's character budget counts body and non-null fact-key JSON
characters; it defers whole records instead of removing their qualifications.
The fixed processor `recall-ledger.evidence/1` is checked inside the Store's
publication transaction, including when invoked by a low-level worker.

Automatic consolidation defaults to `summary_policy="evidence"`. Explicit
`summarize` retains its model behavior; `summary_policy="model"` selects automatic
model summaries. Existing model recipes are claimed only by an exactly matching
processor, and unavailable processors remain pending. The daemon can maintain
evidence recipes without model configuration.

The [full source-preservation contract](fidelity.md) specifies budgets, graph
guards, failure behavior and migration. Verification means exact source
preservation, not semantic truth. Ordinary recall can still return legacy
summaries; never infer strict verification from their source labels.

## Portable extension (development)

| Method | Parameters and behavior |
| --- | --- |
| `bundle_export` | Empty parameters; returns a bounded namespace-complete `recall-ledger.portable/1` envelope |
| `bundle_plan` | `bundle`, `expected_source`; read-only digest, graph, state and synchronization checks; returns counts, conflicts, mappings, `reviewed_digest` and `target_commit` |
| `bundle_apply` | `bundle`, `expected_source`, `reviewed_digest`, `expected_target_commit`, `idempotency_key`; rechecks and atomically applies the reviewed transfer |
| `bundle_context` | `bundle`, `expected_source`, `manifest`; maps evidence only through the latest applied source snapshot and revalidates target content/state |

Bundle limits are 512 KiB of compact UTF-8 JSON and 1,000 records plus candidates.
Source bindings have the same exact namespace/history shape as destination
bindings. Origins survive ID remapping; both the last imported state from this
source and the incoming baseline constrain updates. A higher revision alone
does not prove that independent branches can be merged.

These are trusted administrative SDK/IPC/CLI methods; they are not automatically
added to the model-facing MCP tool list. The existing diagnostic `export` keeps
its earlier semantics. [`migration.md`](migration.md) defines exclusions,
conflict behavior, file commands, current multi-hop limits and recovery.

Python `handoff` implements `recall-ledger.handoff/1` (64 KiB): prepare,
offline inspect, current evidence verification, and explicit cross-ledger
rebase. It carries task prose and evidence references without granting tool
permissions, validating artifact contents or resuming a live execution state.

## Store, runtime, and transports

`Store::open(path)` opens SQLite; `Store::execute(namespace, method, params)`
executes deterministic reads and transactions without model calls.
`runtime::Runtime::new(store, Option<models::Config>)` wraps that store with
configured model execution. Its `execute` method adds `consolidate`, `summarize`, `maintain`,
query embedding, model status, and convenience defaults.

The CLI uses the configured runtime. The daemon pins one namespace and model
configuration for its clients. Model URLs and namespace cannot be overridden in
individual requests. `maintain` is restricted to trusted CLI/embedded runtime
use; socket clients cannot start extra maintenance loops.

IPC uses newline JSON: `{id,method,params,expected_binding?}` returns
`{id,result}` or `{id,error}`.
The frame limit is 1 MiB including the newline. String IDs are limited to 256
UTF-8 bytes before execution. The SDK uses a separate bounded connection per call.
MCP stdio connects to an existing daemon and exposes selected tools.

`binding` with empty parameters returns only `{namespace,history_id}`. The
optional top-level `expected_binding` assertion has exactly those fields and
a hyphenated UUID history ID. It never selects or changes the daemon namespace.
`Store::execute_bound` checks it inside each transaction, before operation
receipts, content reads or writes. `Runtime::execute_bound` propagates it across
model work and checks it again in the final storage transaction. Mismatch
returns nonretryable `binding_mismatch`, with no destination identity or body.
An exact retry cannot obtain a receipt from a differently bound destination.

SDK `Client(..., expected_binding=...)` carries that assertion on every call.
Local adapters persist it at installation and refuse to silently rebind after
restoration or socket reassignment. Legacy SDK callers may omit it; they retain
the old trusted-deployment behavior and lack this misrouting protection.
`recall-ledger mcp --binding-file PATH` pins a saved identity across restarts.
Without the file, MCP reads and pins the identity at startup for that process
only. Neither mode provides authentication against another same-user process.

Errors have `{code, message, retryable}`. Error text from model providers is never
copied into the error response or task metadata. Public low-level administrative
methods remain trusted interfaces; they are not a hostile same-user sandbox.

## Records and idempotency

These methods retain the v0.1 behavior:

| Method | Parameters and behavior |
| --- | --- |
| `remember` | `body`, optional `kind`, `source`, `volatility`, `valid_from`, `valid_until`, and `idempotency_key` |
| `inspect` | `id`, optional `history=false`; ordinary reads require a current valid record |
| `revise` | `id`, `expected_revision`, `body`, `idempotency_key`; increments revision and withdraws dependents |
| `retract` | `id`, `expected_revision`, `idempotency_key`; terminal logical withdrawal of source and dependents |
| `prepare` | `inputs`, `processor`, `idempotency_key`; captures source revisions for a manual processor |
| `job` | `job_id`; returns captured bodies only while those dependencies remain eligible |
| `publish` | `job_id`, `body`, optional `kind`, `idempotency_key`; validates captured dependencies before manual publication |
| `operation` | Original `method` and `idempotency_key`; recovers a committed receipt |
| `export` | Diagnostic current-state snapshot; not a complete backup or restore format |
| `status` | Storage state; configured runtime also reports model keys and indexing state |

Record bodies are limited to 65,536 UTF-8 bytes; derivations have at most 64 inputs.
Mutations and their body-free receipts commit atomically. Reusing a method/key
with different arguments returns `idempotency_conflict`. Empty maintenance-queue
polls have no durable mutation receipt and return `durable=false`.

Runtime `operation` accepts `method="summarize"` as an alias for `summary_create`.
Summary enqueue receipts identify their underlying `receipt_method`.
On an uncertain transport outcome, retry identical arguments and the original
key rather than create another key. An absent receipt alone cannot rule out a
still-running request.

Commit tokens carry database identity and sequence. `minimum_commit` is checked
before query inference and again on the final read. It is not a snapshot held
through a model request or a wall-clock guarantee. Supported restore rotates
history identity and rejects old tokens; manual filesystem cloning does not.

## Candidate admission and fact identity

`candidate_submit`, `candidate_list`, `candidate_inspect`, `candidate_resolve`
and `fact_get` implement [explicit proposal admission](ingestion.md).
All origins begin pending. Fact identity is the exact namespace-local tuple
`{entity,attribute,scope,conditions}`. Accepting an existing identity requires
its current revision, uses the ordinary revision path, and never merges by
vector similarity. Origin and acceptance are trusted caller assertions, not
human authentication or truth verification.

## Change feed and context validation

`changes` takes optional `after` (a commit token) and `limit` (1–500, default 100).
It returns `events`, `cursor`, `has_more`, and `commit_token`. Events commit with
the mutation and its receipt; replaying the same idempotency key adds no event.
They contain method, token and optional primary record IDs, never bodies.
`record_ids_complete=false`: refresh the full context rather than treating IDs
as a complete list of affected descendants.
Keep paging with the returned cursor; a restored/foreign history returns
`commit_history_mismatch`. Initial reads request full resync, including after
an older-schema migration where historical events were not backfilled.

`recall` also returns a `recall-ledger.context/1` manifest containing namespace,
commit token and selected `{id,revision}` references. `validate_context` accepts
that exact `manifest` and checks revisions, current eligibility and transitive
dependencies in one read transaction. It returns `valid`, body-free invalid
references and the current token. It does not verify semantics. Time expiry
does not write a change event, so a quiet feed does not establish freshness.
Previously delivered text remains outside the store's control.

## Backup, restoration and Markdown

Trusted local library/CLI operations provide `backup`, `restore`, `project`
and `read-projection`; they are not filesystem-write tools exposed over MCP.
See [recovery contracts](recovery.md). Restore requires a new destination,
changes history identity and revokes old work. It restores the backup time point,
including content retracted only after that backup. Schema upgrade itself
does not reset history. Native host integration is described in the
[Claude Code](claude-code.md), [local ChatGPT](chatgpt.md), [Codex](codex.md),
[KiroCrew](kirocrew.md), and [OpenCode](opencode.md) guides. Host acceptance and
effective privacy routing remain separate from generating configuration.

## Recall

```json
{
  "query": "implementation language",
  "mode": "hybrid",
  "limit": 10,
  "max_chars": 4000
}
```

`mode` is `lexical`, `vector`, or `hybrid`. Limits are 1–100 records and
0–1,000,000 Unicode scalar values across returned bodies. Query text is at most
4096 UTF-8 bytes. Whole records that do not fit are deferred rather than cut.

Store-only calls default to `lexical`. For semantic calls, trusted callers supply
`model_key` and `query_vector` together with `mode`. The configured runtime can
instead create the vector from `query`; it defaults nonempty queries to `hybrid`
when an embedding provider is configured. Empty default queries stay lexical.

Vector retrieval uses exact cosine similarity over eligible vectors. Hybrid
retrieval applies standard RRF, summing `1/(60+rank)` with 1-based ranks over up to
1000 candidates from each lexical/cosine list. A score never bypasses namespace,
revision, validity, or recorded dependency checks.

`coverage` includes the effective mode, eligible/indexed counts, index completeness,
and budget deferrals. A failed hybrid query-model call returns fresh lexical
results with `degraded=true`, `requested_mode="hybrid"`, and an error-code `reason`.
Vector-only requests fail instead. Missing model configuration is an error for an
explicit semantic request. Coverage is not a claim to know all relevant facts.

Each returned item is marked `trust_boundary="retrieved_data"`. The host must not
promote record text to instructions or authorization.

## Embedding administration

| Method | Parameters |
| --- | --- |
| `embedding_set` | `id`, `expected_revision`, `model_key`, numeric `vector`, `idempotency_key` |
| `embedding_candidates` | `model_key`, optional `limit` |
| `embedding_status` | `model_key`; configured runtime can fill in its current key |
| `embedding_fail` | `id`, `expected_revision`, `model_key`, `error_code`, `idempotency_key` |
| `embedding_retry` | `model_key`, `idempotency_key`; reset recorded indexing failures |

Vectors contain 1–16,384 finite numbers and must have nonzero norm. The first
stored vector binds a namespace/model key to its dimension. Wrong dimensions
are rejected. Embedding writes validate the source revision and dependencies in
the same transaction; revision or withdrawal invalidates affected vectors.
Vectors from different model keys are never combined in one semantic ranking.

The configured provider key hashes its entire configuration. Its mandatory
`revision` is the operator's declaration of loaded weights, not server attestation.
Changing weights under the same declaration cannot be detected reliably.

## Managed summary lifecycle

Configured callers enqueue work with:

```json
{
  "inputs": ["record-id-1", "record-id-2"],
  "max_chars": 2000,
  "idempotency_key": "summary-request-1"
}
```

Call the `summarize` method. It returns `recipe_id`, `task_id`, reserved
`output_id`, `generation`, and `state="pending"`; no generated body is returned.
It requires a configured summary provider. `max_chars` is 1–16,384, default 2000.

A recipe keeps its input IDs, processor identity, and reserved output ID.
Its worker captures current input revisions under a lease, obtains those bodies,
and generates outside SQLite transactions. Publication validates the lease,
source revisions, ancestry, temporal validity, and captured output revision.
Initial publication creates output revision 1; later repairs retain the same ID
and increment the output revision.

`maintenance_status` accepts optional `limit` (1–500, default 100), returning task
metadata, counts, total, and a truncation flag. Tasks are `pending`, `leased`,
`succeeded`, `failed`, or `cancelled`. Status does not return source/model bodies.
It is a bounded oldest-first listing, not a cursor-paginated task explorer.

Source revision withdraws existing outputs, increments task generation, and
revokes any old lease. Retraction cancels affected recipes even before their first
publication. An in-flight model call cannot publish after its lease is revoked.
Model failures remain visible; obsolete outputs do not return while repair fails.
Cancelled recipes' input assignments remain excluded from automatic discovery.
If those source records remain active, an explicit new `summarize` request with a
new key can reserve another output ID without resurrecting the cancelled one.

Low-level trusted worker operations are:

| Method | Parameters |
| --- | --- |
| `summary_create` | `inputs`, `processor`, optional `max_chars`, `idempotency_key` |
| `maintenance_claim` | `worker`, optional `lease_seconds` (1–3600), optional exact `processor`, `idempotency_key` |
| `maintenance_job` | `task_id`, `lease_token`; return only still-valid captured inputs |
| `maintenance_complete` | `task_id`, `lease_token`, `body`, `idempotency_key` |
| `maintenance_fail` | `task_id`, `lease_token`, `error_code`, `retryable`, `idempotency_key` |
| `maintenance_retry` | Failed active `task_id`, `idempotency_key` |

Only managed recipes are automatically repaired. A manual `publish` output still
becomes stale but has no implicit runnable processor. A changed summary model
configuration does not silently repin old recipes.

## Limits and compatibility

The local provider bounds summary inputs, including IDs, to 64 KiB, serialized
requests to 256 KiB, responses to 2 MiB, and per-request timeout to 1–300 seconds.
The store applies the same combined input/ID limit when creating a recipe and
publishing a result; use smaller groups when input limits are reached.
Response model, vector shape, finish status, and output length are
validated. This is protocol validation, not a proof of factual correctness.

Schema 1/2/3 databases migrate transactionally to schema 4 on open. Unknown schema
versions are refused. Older binaries cannot open schema 4. Manual file cloning,
secure physical erasure, ANN scalability, repository-state verification, and
hidden model/tool provenance are outside this contract.
