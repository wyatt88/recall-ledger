# v0.3 validation

Date: 2026-10-10. This is local engineering validation of the source release.
It is not a paper reproduction, semantic-quality benchmark, production
certification, or live Claude conversation acceptance.

## Executed checks

| Check | Result |
| --- | --- |
| Stable Rust, all targets | 133 passed, 0 failed, 0 ignored |
| Minimum Rust 1.89.0, all targets | 133 passed, 0 failed, 0 ignored |
| Python 3.9, SDK and adapter | 37 passed, including 3 actual daemon subprocess tests |
| Formatting | Passed |
| Clippy, all targets, warnings denied | Passed |
| Locked release build | Passed |
| Existing synthetic workflow and grouping example | Passed |

The two Rust runs execute the same test inventory on different toolchains; they
are not 266 independent cases. The seven independently authored adversarial
tests are included in 133, and the three daemon tests are included in 37.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo +1.89.0 test --locked --all-targets
PYTHONPATH=sdk/python RECALL_LEDGER_BIN="$PWD/target/debug/recall-ledger" \
  python3 -m unittest discover -s sdk/python/tests -v
cargo build --release --locked
bash examples/workflow.sh
cargo run --locked --example partition_probe
```

These local runs used macOS on Apple Silicon. CI executes the current source on
Ubuntu and macOS with stable Rust, and Ubuntu with the minimum Rust version.
The CI result for a particular commit must be checked separately from this local
record.

## Coverage of the new paths

- **Ingestion:** exact identity and namespace separation, pending exclusion,
  event replay after restart, concurrent revision conflicts, metadata/validity
  conflicts, rejection and retraction, and inherited vector/summary invalidation.
- **Freshness:** transactional body-free events, pagination, no events on failed
  writes, context revision and dependency checks, and expiry without a mutation.
- **Recovery:** active WAL backup, new-history restore, rejected old tokens and
  leases, no overwrite, sidecar refusal, schema constraints, preserved candidate
  event identities and fact bindings.
- **Projection:** atomic current-generation publication, namespace/history
  binding, live version/expiry checks, offline integrity distinction, manual-edit
  refusal, incomplete generations, interrupted cleanup, and symlink rejection.
- **Claude adapter:** bounded event capture, pending admission, repeated-hook and
  lost-response handling, fresh recall, restored-history refresh, settings
  preservation, install/uninstall recovery, explicit Markdown import, permanent
  failure isolation and retry. Tests use documented hook envelopes and real
  RecallLedger daemon processes, not a live Claude runtime.

The [independent review](REVIEW-v0.3.md) provides seven cross-module
counterexamples and follow-up findings. [Recovery notes](recovery.md) record
the additional filesystem review.

## Findings corrected before release

1. An absent destination database could have a stale same-name WAL. Restoration
   now refuses any existing database or associated SQLite sidecar.
2. Column-only schema comparison could miss altered constraints. Recovery now
   checks normalized schema definitions as well as table metadata and integrity.
3. Interrupted old-generation cleanup could leave an apparently published but
   incomplete directory. Retiring a generation now uses a separate GC name
   before deleting its contents.
4. File checksums do not establish memory freshness. Default projection reads
   now check the live database and time eligibility; offline integrity inspection
   requires an explicit option.
5. Uninstall could stop between writing settings and its completion receipt.
   A persisted intent now supports retry against either exact expected state.
6. Blank import chunks and other permanent candidate errors could block the
   entire outbox. Import skips blank chunks; permanent failures retain their
   original requests in an inspectable dead-letter area.

A new malformed-schema fixture initially expected only `schema_mismatch`;
SQLite rejected the malformed autoindex earlier with `database_error`.
The assertion was corrected to accept either explicit fail-closed result.
The failure was in the overly specific test expectation, and was not hidden as
a first-run pass. Final full runs passed after this correction.

## Not established

No model calls or user-memory migration were performed in this release's tests.
The v0.2 local-model smoke remains a separate historical result.
No real Claude installation was modified. Live host reload/compaction behavior,
model compliance with memory corrections, cross-host interoperability, learned
extraction, summary fidelity, ANN scale, power-loss durability and hostile
same-user filesystem races remain unverified.

Explicit acceptance is a trusted API action, not authenticated human review.
Fact keys are declared identities. Restore is point-in-time recovery and cannot
retain changes absent from the backup. Retraction does not erase external
copies, original imported files, prior exports or already delivered prompts.
