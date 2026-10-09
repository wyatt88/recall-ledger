# Changelog

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
