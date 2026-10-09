# Testing and evidence boundaries

The tests exercise the public `Store::execute` contract using fresh temporary
databases. They do not mock the Store, reproduce its SQL, or substitute a second
invalidation implementation as the expected result.

```sh
cargo test --test core_contract --test property_sequences
cargo test
```

`core_contract.rs` checks:

- Idempotent retries, conflicting keys, and recovery of a lost response.
- Namespace boundaries for records, operations, prepared jobs, and publication.
- Revision compare-and-swap across independent SQLite connections and two
  simultaneously released writers.
- Three derived levels becoming unavailable after a source revision.
- Late publication after a source revision, retraction, or wall-clock expiry.
- Conservative withdrawal of mixed free text even when another input survives.
- Retention of dependency obligations when a derived record is revised.
- No withdrawn bodies through current reads, history, export, jobs, errors, or
  operation receipts.
- Single-use publication with recovery of an exact retry.
- Current validity windows and database-specific minimum-commit receipts.
- CJK substring lookup, Unicode character budgets, and literal query handling.
- Restart persistence of readable records, jobs, receipts, and retractions.

`property_sequences.rs` contains deterministic metamorphic scenarios. It builds
1–8 repeated diamond levels in each of revision and retraction variants (16
scenarios), then closes and reopens the database before checking withdrawal and
late publication. The independent branch must stay readable; refusing everything
does not pass. A second scenario repeatedly restarts and replays all committed
writes in reverse order to detect duplicate side effects. These are bounded
construction-based properties, not exhaustive property-based fuzzing.

One expiry test waits until a two-second validity window has passed. It checks
time again rather than relying on a single exact sleep duration. The remaining
fixtures use wide time margins and require no model, network, credentials, or
GPU. All databases are temporary and removed by the test harness.

## What passing means

Passing supports the particular local contract scenarios on the tested platform
and build. It does not establish:

- Safe erasure from SQLite pages, WAL files, device blocks, or backups.
- Protection from a hostile process with the same OS identity.
- Recovery from arbitrary power loss, file corruption, or rollback to old copies.
- Awareness of input a processor read outside the prepared job.
- Semantic correctness of generated summaries or improved Agent task success.
- A reproduction of any cited paper or readiness for every deployment environment.

Reproducible paper experiments and performance measurements require separate,
frozen data, baselines, budgets, hardware details, and reports. No test count here
is a performance or research-quality benchmark.

## Execution record

On 2026-10-10 (Asia/Shanghai), the independently authored contract tests were run
against the actual Store on macOS 27.0.1 / arm64:

```text
rustc 1.99.0 (b940084d7 2026-09-28), aarch64-apple-darwin
cargo 1.99.0 (5f94df478 2026-08-27)
cargo test --test core_contract --test property_sequences
core_contract:      18 passed; 0 failed; 2.58s
property_sequences:  2 passed; 0 failed; 0.24s
```

These durations describe this test run, not product latency measurements. The
first integration run exposed two test fixtures that passed extra fields to
methods with strict argument schemas. The fixtures were corrected to issue valid
requests before checking namespace isolation. No production failure was hidden
by weakening an assertion.

The accompanying static review checked mutation/receipt transaction boundaries,
revision comparison, ancestor validation, preservation of edges during a derived
record revision, and every public body-read path. It found two release issues
outside those passing assertions:

1. The initial `single` and `fixed` planning strategies used the same grouping
   logic. They must not be presented as distinct algorithmic baselines. The
   implementation owner accepted replacing `single` with explicit `individual`
   groups and retaining `fixed` as bounded grouping.
2. The initial manifest declared Rust 1.85, but locked `uuid` 1.27.0 declares
   Rust 1.89. Dependency metadata was checked with `cargo metadata --locked`.
   The published minimum must match dependencies and be verified separately;
   this run itself tested Rust 1.99 only.

Later changes require rerunning affected tests. This record is local engineering
validation, not a paper reproduction or a deployment acceptance certificate.

## Integrated release checks

The final local integration on 2026-10-10 passed on macOS arm64:

| Check | Observed result |
| --- | --- |
| Rust 1.99.0, `cargo test --locked --all-targets` | 35 tests passed |
| Minimum Rust 1.89.0, same command | 35 tests passed |
| Formatting and Clippy with `-D warnings` | Passed |
| Python client transport unit tests | 7 passed |
| Release build and `examples/workflow.sh` | Passed |
| Python client against the real release daemon | Write, recall, stale-publication rejection, and example passed |
| `cargo run --locked --example partition_probe` | Both synthetic change scenarios completed |

The 35 Rust tests comprise 18 core-contract tests, 2 bounded graph/replay tests,
4 planning tests, 8 real-process transport tests, and 3 MCP unit tests.
Transport coverage includes daemon restart, parallel clients, namespace pinning,
oversized frames/IDs, and deliberately lost committed receipts. The planning
tests include the mechanism example, grouping boundaries, Unicode budgets, and
the 64-input cap. See [REVIEW.md](REVIEW.md) for the independent review and the
oversized-ID correction.

The lost-receipt test now keeps the originating connection unread until a second
connection confirms the commit, then discards the original connection. Its first
form disconnected immediately and incorrectly assumed the request must already
have committed. That invalid fixture assumption caused an integration failure;
the correction preserves receipt equality and the single-write assertion.

GitHub Actions separately runs on Ubuntu and macOS, plus the minimum Rust
toolchain on Ubuntu. Consult the run attached to a particular commit for remote
results; the local observations above do not substitute for a remote CI result.
