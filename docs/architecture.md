# Architecture

RecallLedger 0.1 is a local, revision-aware record store. Its core responsibility
is deciding whether a result derived from known inputs may be published and
read. It does not decide whether the result is factually true.

## Deployment

```text
Agent host
  ├─ Python SDK ──────────────┐
  └─ MCP host → stdio adapter ├─ Unix-domain socket → daemon → Rust Store → SQLite
                             │
Trusted CLI / Rust caller ────┴─ direct Store access

External processor: reads prepared inputs and submits a proposed derived body
```

The MCP adapter connects to the running daemon. It is not a second memory store.
One daemon owns a database lock; the trusted namespace configuration binds socket
requests to a namespace. Direct library and CLI calls are administrative access.

The MCP adapter targets protocol version `2025-11-25`, supporting tool calls only,
with no sampling or task capabilities. Clients must complete initialization and
send `notifications/initialized` before listing or calling tools. `--db` and
`--namespace` do not change the daemon selection in MCP mode.

There is no remote transport, model dependency, GPU requirement, or HTTP service.
The library uses portable Rust/SQLite components; the 0.1 daemon transport targets
macOS and Linux. Windows daemon behavior is not provided.

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

The first implementation does not retain an affected free-text summary merely
because some other source could support a similar conclusion. It withdraws the
summary and allows an explicit fresh job to create a replacement.

## Transactions, retries, and commit tokens

Every mutation requires an idempotency key. The operation, argument fingerprint,
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

The 0.1 protocol does not support file-level cloning or rollback restoration as
a new storage history. Do not infer restore safety from commit tokens. `export`
is a diagnostic view of currently readable state, not a restorable backup.

## Retrieval

The first retrieval path stays deterministic and local:

1. Bind the namespace from trusted configuration.
2. Find candidates using literal-token text search, with a CJK fallback.
3. Enforce current revision, validity, and dependency visibility.
4. Return records within count and character limits, with budget and coverage
   information.

`max_chars` is a character budget. It is not a model-token estimate. Unicode text
and individual model tokenizers have different token counts.

This version has no semantic embeddings, RRF, reranker, graph similarity search,
or learned relevance score. Eligibility and ranking are separate concerns:
a high retrieval score must never override retraction or stale dependencies.

## Experimental consolidation groups

`plan` proposes groups; it does not call a model or create a summary. The
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

Features that are not supplied by 0.1 include:

- Explicit migration, backup, and restore protocols.
- Durable automatic repair queues and cost-aware repair selection.
- Optional vector retrieval and ranking fusion.
- Captured repository and tool-state preconditions for coding agents.
- Alternative support sets for structured claims.
- Learned write/read policies and long-horizon task evaluations.

Each addition must preserve the current publication and withdrawal contract.
Research inspiration is not an implementation or performance guarantee.
