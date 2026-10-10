# v0.2 engineering review

Review date: 2026-10-10. This is an independent AI-assisted source and contract
review, not a security certification, a paper reproduction, or evidence of
improved Agent task quality. Synthetic vectors and loopback HTTP stubs are
identified separately from any real-model checks in the release record.

## Scope

The review covers the transition from the released v0.1 schema and manual
derivation workflow to:

- revision-bound, model-scoped vectors and cosine/lexical retrieval;
- persisted summary recipes, leased work, conditional completion, and repair;
- explicit local model configuration and bounded model HTTP requests;
- background indexing and summary execution when the daemon is configured.

The reviewer authored `tests/vector_contract.rs` and
`tests/runtime_contract.rs` independently of the vector/runtime implementation.
Queue and provider tests were authored by their implementation owners and
reviewed as supporting checks; this distinction matters when assessing test
independence.

## Risks addressed before integration

**Claim receipts must not preserve source bodies.** The existing idempotency
ledger retains receipts so that callers can recover a committed operation.
Putting source text in a `maintenance_claim` receipt would have created another
body-read path after retraction. Claims therefore return lease and dependency
metadata only. `maintenance_job` reads bodies separately and revalidates the
lease, captured revisions, ancestry, temporal validity, and output revision.
Replaying an old successful receipt does not authorize another current read.

**Repair must not overwrite an independently edited output.** Rechecking source
versions alone cannot detect that another writer edited the output while the
model was running. Claims capture the output revision as well. Managed summaries
reject direct `revise`; their inputs are revised instead. Conditional completion
also checks the captured output revision. Manual v0.1 derivations keep their
existing editing contract.

**A vector is tied to an encoding space and a source revision.** Equal vector
length does not establish equal meaning. Embedding model keys include the full
explicit provider configuration and declared weight revision. The store binds
each namespace/model key to one dimension, even after its last indexed record
is withdrawn. Late embeddings for a changed or no-longer-readable source fail.
Revision changes, dependency invalidation, and retraction remove old indexes;
retrieval independently rechecks record and ancestor validity.

The declared model revision is supplied by the operator. The model protocol
does not attest the served weight files. Operators must change the revision
when the served weights or encoding policy change.

**Dependency recovery must not skip an unavailable intermediate artifact.**
Managed repair keeps the existing output identity while advancing its revision
and captured parent revisions. Consumers stay unavailable until their own
dependencies are ready. A managed summary → manually produced artifact →
managed summary chain cannot automatically cross the manual artifact: it
requires the manual producer to publish a valid replacement and create the
downstream recipe using that replacement's identity.

## Independent vector and migration checks

The 12 independently authored vector/migration tests passed on the reviewed local
implementation. They exercise:

| Check | What it establishes |
| --- | --- |
| Positive, orthogonal, and negative fixed vectors | Direction-based ordering, independent of vector magnitude or word overlap |
| Lexical-only, vector-only, and hybrid searches | Hybrid uses the union of independently ranked channels; a shared hit receives rank-fusion credit |
| Equal dimensions in separate model spaces and namespaces | Data is not searched across the selected model key or namespace |
| Mixed dimensions and malformed/zero vectors | Invalid input fails without a successful mutation receipt |
| Very large and very small finite components | Normalization and returned ranking scores remain finite in the exercised cases |
| Revision changes and a late indexing request | Old vectors disappear and an outdated embedding cannot be committed |
| Three derived levels and retraction | Vector reads respect transitive invalidation; an old receipt cannot restore withdrawn text |
| Ancestor expiration with no intervening write | Time is rechecked during vector retrieval and embedding submission |
| Candidate limits, expired/future/stale sources | The indexing scan emits bounded, currently readable records |
| Reopening a database | Indexed values and idempotency receipts survive the exercised restart |
| Two simultaneous opens of a populated v0.1 database | Migration preserves storage identity, commit sequence, old receipts, history, FTS, and prepared jobs |
| An unknown future schema | The database is rejected without creating current tables |

The migration fixture reproduces the released v0.1 table definitions with
synthetic data. It is not a fresh v0.2 database with only its version number
changed. Migration takes a write transaction, rereads the schema version while
holding that transaction, and creates the new schema before committing version 2.
No downgrade or arbitrary backup-rollback support is implied.

## Model and queue boundaries

Provider source review found explicit loopback addresses, no DNS-based arbitrary
hostnames, no proxy use, no redirects, bounded request/response sizes, configured
timeouts, and no implicit remote provider fallback. A complete explicitly
configured path is allowed so compatible local servers can expose their normal
endpoints.

Malformed output, zero/nonfinite embeddings, model-identity mismatches,
truncated summary completions, tool-call responses, and over-budget summaries
are rejected. Model errors are returned as bounded machine codes rather than
copying the provider's response body. Source records are delimited as untrusted
data in the summary prompt. This reduces an avoidable prompt-boundary mistake;
it does not prove factual grounding or injection immunity.

Queue source review checks persisted failure counts, bounded backoff, expired
lease reclamation, fencing of old lease holders, namespace checks, metadata-only
status/receipts, and terminal cancellation after retraction. Queue completion
and source/index updates share the store transaction. Model execution occurs
outside that transaction.

The reviewer independently reran the implementation owners' queue and provider
suites: **14 queue tests and 13 loopback HTTP stub tests passed**. These reruns
verify the checked local snapshot; they are not additional independent test
designs or real-model executions.

## Integrated runtime checks and error-path repairs

The **10 independently authored runtime tests passed**, using actual loopback
HTTP requests to deterministic synthetic peers:

- Configured indexing issues document requests and a semantic query issues its
  own query request. Already-current vectors are not regenerated.
- Queued summaries are generated, repaired under the same output ID with a new
  revision, and stop generating after source withdrawal.
- Automatic grouping does not repeatedly summarize its own outputs.
- Pending work survives reopening the runtime with the same provider identity.
- A deliberately blocked summary response allows another database connection
  to revise its source during inference; the outdated result cannot publish.
- A changed summary configuration cannot silently execute an old pinned recipe.
- A failed embedding provider gives an explicitly marked lexical degradation
  for hybrid requests; explicit vector requests fail.
- Missing providers fail explicitly while ordinary lexical memory remains usable.

Two additional error paths found during source review were repaired and covered
by independent counterexample tests:

1. **Embedding commit failure must also respect backoff.** If the configured
   dimensions are unspecified and a provider changes its returned dimension,
   HTTP generation can succeed while the store rejects the vector. Recording
   only an error would repeat the model work on every tick. A current-source
   dimension/invalid-vector commit failure now enters the persisted retry path.
   The test changes a synthetic provider from two to three dimensions and checks
   that the next immediate tick makes no additional request.
2. **Withdrawal is not a new automatic work item.** After an automatically
   generated output was withdrawn, its raw inputs could become eligible for
   discovery again. Replaying the original idempotency receipt could then be
   misleadingly reported as newly queued work. Automatic assignment now includes
   cancelled recipe history. The test checks that another tick neither generates
   nor reports a new queue item. An explicit new summary request remains available
   when the caller intentionally wants a new output.

The CLI/daemon source review found one configured maintenance thread per daemon,
no model call while holding a store transaction, bounded work per tick, and
client-supplied `maintain` rejected at the IPC boundary. A summary lease includes
the configured model timeout plus a 30-second margin. Socket/client request
timeouts are 330 seconds to cover the provider's supported 300-second maximum.
These limits do not bound the entire duration of a maintenance batch or prove a
latency target under competing local model load.

Automatic groups canonicalize input IDs before constructing their deterministic
request and idempotency key. This prevents a different planning order for the
same input set from creating a different automatic key. Explicit user-created
recipes with new keys remain separate requests. This change was source-reviewed;
it is not a proof against every concurrent discovery interleaving.

## Read-only review of the owner's real-model run

The reviewer subsequently inspected the owner's local protocol, execution script,
saved responses, inference-call ledger, and binary checksum. The reviewer did
**not independently rerun the real models**.

The recorded integrated run passed eight checks with seven embedding requests
and two summary requests. Including the two earlier exploratory provider calls,
the recorded total was 11 of the 20-call budget. The serving responses identified
`embeddinggemma-2` and `gemma4:31b-mlx`; these names and declared revisions do not
independently attest the model weights. At review time, the saved binary checksum
matched the local release executable.

For the synthetic project facts, the stored output changed from an April launch
to an August launch under the same summary ID, advanced to revision 2, and
referenced revision 2 of the changed source. The saved record also showed terminal
recipe cancellation after source withdrawal and no summary reappearance after
daemon restart.

Two limits are essential when using this record:

- The paraphrase test requested three results from a store containing three
  readable records, and lexical retrieval also returned all three. Although the
  relevant SQLite source ranked first in that vector response, this acceptance
  test does not discriminate retrieval quality or establish an advantage over
  lexical search.
- Post-withdrawal and post-restart checks used `inspect`, queue status, and
  lexical recall. They did not issue another real-model vector query. Vector
  withdrawal protection is supported separately by source review and the
  synthetic vector contract tests, not by that particular live-run assertion.

The record supports a successful bounded local integration scenario. It does
not establish Agent task gains, general summary accuracy, latency guarantees,
or an independently reproduced model benchmark.

## Limits that remain material

- Exact cosine search scans readable records and their stored vectors. It is
  not an approximate-nearest-neighbour index or a large-store latency guarantee.
  Hybrid fusion uses reciprocal ranks with `k = 60`, capped at 1,000 candidates
  per channel. This choice has not been tuned against a held-out quality set.
- Indexing may be incomplete, waiting for a local model, or blocked by a
  provider error. Retrieval reports index coverage; a semantic query cannot
  recover facts that have not been indexed.
- Automatic summaries can omit details or introduce mistakes. Revision and
  dependency checks establish which source versions were supplied, not that
  every output claim follows from those sources.
- A summary recipe has fixed input identities. Repair refreshes those inputs;
  it does not infer a new task, choose new evidence, or repair an arbitrary
  manually written artifact.
- Retraction prevents future reads through the managed APIs. It cannot retract
  context already delivered to an Agent or local model, erase provider logs, or
  guarantee physical deletion from SQLite pages, WAL files, or backups.
- Local endpoints and namespaces do not isolate hostile programs running under
  the same OS account. The configured model process remains part of the trust
  boundary.
- Clock-based lease expiry, bounded tests, and local restarts are not a proof of
  crash consistency under every power-loss, filesystem, or corruption scenario.
- No Agent benchmark, paper reproduction, independent semantic-quality
  evaluation, memory reduction claim, or production-scale performance result
  follows from these engineering tests.

No remaining release blocker was identified within this reviewed local scope.
The implementation owner must run the complete integrated checks on the final
release commit. A reviewed module snapshot and an inspected owner-run record do
not establish that later integration changes, real semantic quality, or public
CI have passed.
