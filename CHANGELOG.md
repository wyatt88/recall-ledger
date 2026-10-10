# Changelog

## 0.2.0 — 2026-10-10

- Add revision-bound embedding storage, exact cosine retrieval, and hybrid recall
  using reciprocal-rank fusion with `k=60`.
- Add optional loopback model configuration with explicit model revisions,
  bounded HTTP requests, and no proxy, redirect, or cloud fallback.
- Generate managed summaries from captured inputs using a local model.
- Persist summary recipes and leased tasks; withdraw obsolete outputs before
  regeneration and publish repairs with revision checks and stable output IDs.
- Cancel affected managed recipes when a source or output is retracted.
- Run bounded indexing, consolidation, and repair work with the daemon; expose a
  manual maintenance command and task-status inspection.
- Extend the Python client with summary enqueue and maintenance-status methods.
- Report index coverage and explicit lexical degradation when hybrid query
  embedding fails. Vector-only requests return the error.
- Migrate the version 1 storage schema transactionally to version 2 on open.

This is a source preview. Exact cosine search is not an ANN index, and automatic
repair applies to managed recipes rather than arbitrary manual derivations.
Model-generated summaries are not fact-checked. No benchmark advantage,
production readiness, or paper reproduction is claimed.

## 0.1.0 — 2026-10-10

- Add a Rust library, CLI, and local SQLite record store.
- Track record revisions, validity windows, and explicit derivation inputs.
- Add transactional preparation and publication with stale-input rejection.
- Withdraw dependent records after source revision or retraction.
- Add durable idempotency receipts and receipt lookup.
- Add local text recall with character budgets and experimental grouping rules.
- Add a Unix-domain socket daemon, MCP stdio adapter, and Python client.
- Include synthetic workflow examples, regression tests, and macOS/Linux CI.

This initial version does not include embedding models, semantic summarization,
automatic repair scheduling, or claims of paper reproduction or agent quality
improvement.
