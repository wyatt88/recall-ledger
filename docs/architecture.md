# Architecture

RecallLedger 0.2 is a local, revision-aware record store with optional local model
execution and durable maintenance. Its core responsibility
is deciding whether a result derived from known inputs may be published and
read. It does not decide whether the result is factually true.

## Deployment

```text
Agent host
  ├─ Python SDK ──────────────┐
  └─ MCP host → stdio adapter ├─ Unix-domain socket → daemon → Rust Store → SQLite
                             │
Trusted CLI / Rust caller ────┴─ direct Store access

Daemon worker → current records → local embedding endpoint → revision-bound vectors
              → durable recipes → local chat endpoint → guarded summary publication

External processors can still use the manual prepare/job/publish protocol.
```

The MCP adapter connects to the running daemon. It is not a second memory store.
One daemon owns a database lock; the trusted namespace configuration binds socket
requests to a namespace. Direct library and CLI calls are administrative access.

The MCP adapter targets protocol version `2025-11-25`, supporting tool calls only,
with no sampling or task capabilities. Clients must complete initialization and
send `notifications/initialized` before listing or calling tools. `--db` and
`--namespace` do not change the daemon selection in MCP mode.

The Agent transport has no HTTP listener. Optional model calls go only to
explicitly configured loopback HTTP endpoints, with proxies and redirects
disabled. Models and their CPU/GPU resources are supplied separately.
The deterministic Store works without a model. The daemon targets macOS and
Linux; Windows daemon behavior is not provided.

## Records and revisions

A record carries body text, a kind, source metadata, volatility, a revision,
namespace, and optional validity bounds. Kinds distinguish assertions,
inferences, preferences, episodes, commitments, and summaries. A type is an
interpretation label, not a factual verification result.

`revise` requires `expected_revision`. A successful update increments the
revision and withdraws transitive derived descendants in the same transaction.
Superseded revisions are historical data; ordinary recall does not serve them as
current facts.

`retract` requires the same optimistic revision check. Retraction blocks
subsequent managed body reads, including history, job inputs, and export, and
clears managed body content. It does not securely erase underlying SQLite pages,
WAL files, backups, or copies already sent to clients.

A validity window is supplied by the caller. Passing a time check says only
that the declared window currently permits use. It does not discover real-world
changes or validate the content.

## Prepare and publish

Processing can be slow and can run outside the service. The input snapshot and
publication decision therefore need an explicit protocol:

1. `prepare` checks the selected sources and captures their identities and
   revisions in a transaction.
2. `job` returns source bodies only while the captured dependencies remain
   readable and valid.
3. The external processor produces a proposed body.
4. `publish` validates all captured revisions and their ancestry in a transaction.
5. On success, that transaction creates the derived record and dependency edges,
   and marks the job published. If inputs changed, publication fails.

An illustrative interleaving:

```text
worker: prepare(A@1) ───── process ───────────────────── publish → rejected
client:                      revise(A@1 → A@2)
worker:                                          prepare(A@2) → publish → accepted
```

The guarantee is about the publication and read transaction ordering. It cannot
remove text already in a client's context. It also cannot stop a source from
changing again after a valid result has been delivered.

Recorded dependency edges are conservative influence edges: an output depends
on the inputs the processor received. They are not similarity edges, citations
that prove a claim, or causal explanations of every output token. The processor
must be trusted to use only the supplied inputs, or report every additional
source through a future extension. Hidden reads invalidate claims of complete
provenance.

The implementation does not retain an affected free-text summary merely because
some other source could support a similar conclusion. It withdraws the summary.
Manual derivations require a new explicit job; managed summaries use the repair
queue described below.

## Managed summaries and repair

The configured runtime adds a `summarize` operation. It records a recipe with
input record IDs, a pinned processor configuration, an output character budget,
and a reserved stable `output_id`. The enqueue receipt is durable; generation
does not happen in its SQLite transaction.

A worker claims a task under a bounded lease, captures current source revisions
and the current output revision, reads those captured inputs, and calls the local
summary model outside all database transactions. Publication checks the lease,
captured input revisions, ancestor validity, and output revision again. A late
result cannot replace a newer or withdrawn result.

Source revision withdraws the affected result and invalidates outstanding
leases before scheduling a fresh task generation. A successful repair updates
the same output record with a higher revision and fresh dependency edges.
Dependent managed recipes are also queued. Recipes whose inputs are currently
stale must wait for their upstream results to become valid.

Retraction is terminal: affected recipes become cancelled, including recipes
that have not published yet. An already running HTTP request may finish, but its
old lease cannot publish. No mechanism retracts input text already delivered to
the model server.
Automatic discovery also excludes inputs assigned to cancelled recipes, avoiding
silent recreation of a withdrawn summary. A deliberate new `summarize` request
can create a different output from source records that remain active.

Tasks and retries persist in SQLite. Expired leases can be reclaimed after a
worker dies. Recorded model failures have bounded attempts and backoff; failed
tasks remain visible until an explicit retry or relevant source change.
Neither an error nor a delayed repair makes obsolete text readable again.

The default automatic grouping policy considers eligible unmanaged root records,
preserves source/kind boundaries, and creates recipes only for bounded groups of
at least four records. It uses deterministic grouping, not learned importance.
Explicit `summarize` may use fewer records. `--no-worker` disables all background
maintenance; `--auto-summary-min 0` disables only automatic grouping.

Only managed recipes are regenerated. The system cannot infer how to rerun an
arbitrary external processor that used the manual `publish` operation.

## Transactions, retries, and commit tokens

Every Store mutation requires an idempotency key. The operation, argument fingerprint,
and result receipt are committed together. The receipt stores identifiers and
status, not the original body.

- The same namespace, method, key, and arguments return the original receipt.
- A reused key with different arguments returns a conflict.
- `operation` can recover a receipt when the client lost the original response.
- Unknown receipt lookup is not proof that an in-flight request cannot later
  commit. A client should retry the same operation with the original key.

The Python client does not retry requests automatically. A
`TransportError.outcome_unknown` value of `true` means a request may have reached
the daemon even though its reply was lost. Recover the receipt with the original
method and key, or retry identical arguments and the original key. Creating a
fresh key can duplicate an operation.

Commit tokens identify storage and commit sequence. `minimum_commit` allows a
read to require a known committed position. Tokens are not wall-clock time or a
distributed consensus protocol.

The protocol does not support file-level cloning or rollback restoration as
a new storage history. Do not infer restore safety from commit tokens. `export`
is a diagnostic view of currently readable state, not a restorable backup.

## Retrieval and model identity

The Store supports three retrieval modes:

- `lexical`: literal-token text search with a CJK fallback.
- `vector`: exact cosine scores for vectors matching the selected model key and
  current record revision.
- `hybrid`: independently rank eligible lexical and cosine candidates, then sum
  `1 / (60 + rank)` over the two ranked lists. Each list contributes at most 1000
  candidates. A missing vector does not act as a zero-score vector.

Eligibility is enforced for both channels before selection. Namespace, current
revision, validity, and dependency checks cannot be bypassed by a high score.
Returned records also obey count and whole-body character limits.

The Store receives caller-supplied query vectors and performs no network I/O.
The configured runtime obtains query vectors from the local model. With an
embedding provider, nonempty queries default to hybrid; without one, recall
defaults to lexical. Hybrid query-model failure permits an explicitly marked
lexical degradation; vector-only failure returns the error.

An embedding is identified by namespace, record ID, source revision, and a stable
model key. The key hashes the complete embedding configuration: endpoint, model,
operator-declared weight revision, dimensions, task field, and timeout. A changed
configuration uses a separate vector space. Matching dimensions alone are not a
space identity check. The endpoint's returned model and dimensions are validated,
but the protocol cannot attest the actual weights behind a reused server alias.

Revision or retraction removes affected active embeddings. Workers publish a
vector only if its captured source revision is still valid. Indexing is
asynchronous; recall reports eligible/indexed counts and incomplete coverage.
An empty semantic result does not prove the database contains no relevant facts.

Vectors live in the same SQLite database as records. This version uses an exact
scan, not an ANN index, quantized search engine, or measured large-scale service.

`max_chars` is a character budget. It is not a model-token estimate. Unicode text
and individual model tokenizers have different token counts.

This version has no reranker, graph similarity retrieval, or learned relevance
policy. Ranking and lineage management do not establish factual truth.

## Experimental consolidation groups

`plan` itself proposes groups without calling a model or creating a summary. The
strategies `individual`, `fixed`, `topic`, and `volatility` support later
comparisons while preserving namespace, source, and kind boundaries and bounded
group text.

`individual` keeps one record per group. `fixed` fills bounded groups in
deterministic order. `topic` buckets by the first whitespace-delimited token; it
is a lexical heuristic, not a semantic topic model. `volatility` groups
caller-provided labels rather than learned change forecasts. Oversized records
are reported separately. There is no demonstrated compression, repair-cost, or
task-quality advantage. See [research.md](research.md) for the hypothesis and
its falsification conditions.

The executable [partition probe](../examples/partition_probe.rs) demonstrates
how these grouping rules change the set of derived records withdrawn after
source revisions:

```sh
cargo run --locked --example partition_probe
```

It uses 12 synthetic records in temporary databases and real
`prepare`/`job`/`publish`/`revise` calls. Fixture-only SQL fixes source ordering;
derived bodies are verbatim concatenations. Its record and character counts
illustrate dependency coverage and a trade-off in initial artifact counts.
They are not token costs, rebuild measurements, latency results, or evidence
of semantic quality. The accompanying all-sources-change case prevents treating
volatility grouping as universally better. See the
[observation table](research.md#executable-mechanism-example).

## Integrating existing agent memory

An agent framework can use RecallLedger as its long-term store backend. A host
should keep execution checkpoints in the system that understands its workflow
state, and should report relevant observations to the memory store explicitly.

Choose one authority for a given long-term record. If the host maintains an
independent profile copied from the store, that copy has a separate revision and
retraction problem. An MCP-only integration cannot enforce changes inside an
uncooperative host.

The core can enforce its local contract without a model. Whether using the
resulting memory improves task outcomes requires a separate comparison with a
strong native-store baseline, equal processing budgets, and independent tasks.

## Future work

Features that are not supplied by 0.2 include:

- General backup, restore, and clone protocols. Schema 1 to 2 migration is present.
- Cost-optimal repair selection and automatic reruns of arbitrary manual processors.
- ANN indexing, rerankers, and graph similarity retrieval.
- Captured repository and tool-state preconditions for coding agents.
- Alternative support sets for structured claims.
- Learned write/read policies and long-horizon task evaluations.

Each addition must preserve the current publication and withdrawal contract.
Research inspiration is not an implementation or performance guarantee.
