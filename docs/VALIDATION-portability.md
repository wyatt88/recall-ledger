# Portable migration and handoff: development validation

Date: 2026-10-11. Base commit: `ac39ce51b80e7067d43306509e3c7fb5fd55893f`
(v0.4.0). At the time of this validation, the changes were uncommitted
development source. This record describes those checks, not a new published
tag. Rust/Python package labels remain 0.4.0 during development.
Database schema is now 4. The v0.4.0 tag does not contain these features.

Platform: macOS 27.0.1 arm64. All memory used for this work was synthetic and
temporary. No model calls, user-memory import, real host configuration changes,
public deployment or paid inference were performed.

## Executed checks

| Check | Result |
| --- | --- |
| Rust 1.99.0, `cargo test --locked --all-targets` | 175 passed; 0 failed; 0 ignored |
| Minimum Rust 1.89.0, same command | Same 175 cases passed; not additional independent cases |
| New portable contract suite | 23 passed, included in the Rust total |
| New file CLI and schema-upgrade suite | 5 passed, included in the Rust total |
| `cargo fmt --all --check` and all-target Clippy with `-D warnings` | Passed |
| Python 3.12.14 full suite with actual daemon and built wheel | 113 passed; no skipped tests |
| New handoff tests | 14 cases included in Python total; 4 actual-daemon/CLI scenarios, 10 bounded SDK/shape tests |
| Wheel installation outside the checkout | Existing actual bridge test passed; new `recall-ledger-handoff` entry point loads and exposes verify/rebase |
| Node v26.10.0 OpenCode protocol fixtures | 8 passed; unchanged protocol fixtures, not OpenCode app acceptance |
| `examples/migration_handoff.py` | Two actual daemons complete A → B → A, preserve pending state and root identity, reject stale handoffs, propagate withdrawal |
| Independent adversarial probes | Two reproduced defects repaired; four cross-module probes and the final replay boundary passed |

The Python suite's final observed duration was 7.697 seconds. Durations are test
run observations, not product latency measurements. Hosted CI was not run for
this working tree; its workflow now includes the migration example.

## Defects discovered and repaired during review

**Divergent branches.** A → B/C, then B revision 2 → A, then independently
edited C revision 3 → A initially overwrote B. The importer had treated the
destination's last import from any source as causal evidence. It now records
last-imported object fingerprints per source binding. A larger revision alone
cannot merge a divergent branch. The original counterexample and a permanent
three-store regression now reject the import and retain B's decision.

**Restored old snapshots.** A native record's inferred origin originally
followed the current database history. Restoring an old backup changed that
history, allowing the old body to appear as a new origin at a destination that
already knew its withdrawal. Restore now freezes native record and candidate
origins across all namespaces before rotating the transport history, in the
same transaction. Unknown synchronization baselines stay null. The original
counterexample now rejects the old identity; target recall stays empty.

**Replay semantics.** Reapplying the identical imported snapshot with a fresh
key is a no-op for memory objects even after local withdrawal. It still creates
a new operation receipt/event and advances the commit sequence. Old handoff
references remain invalid. Reusing the original key returns its original receipt.

Independent review used pinned executable copies to avoid observing concurrent
builds. The final narrow replay check used SHA-256
`6760113fc30bb89eed5215f783f0f381cad13ef6015dd9fd17bb0947411cc5ee`.
The subsequent full local checks above tested the same final source.

## Tested boundaries

The contract tests cover digest/shape rejection, source and target binding,
exact target-commit CAS, graph closure and cycles, transaction rollback,
namespace isolation, repeated imports, process reopening, pending/accepted/
rejected/withdrawn state, fact/event collisions, source revision propagation,
target-only derived records, stale vectors, tombstone history erasure and
restore-origin preservation. The restore pinning test exceeds the portable
1,000-object limit to ensure ordinary recovery does not depend on export limits.

Handoff checks include JSON round trips, unknown fields, tampering, size limits,
duplicate references, private/exclusive files, wrong binding, expiry, daemon
replacement, source-to-target reference remapping, corrections, withdrawals,
original-packet preservation and absence of automatic execution authority.

## What remains unvalidated or unsupported

- Actual ChatGPT/Codex/KiroCrew/OpenCode task transfer, warm-context cutover and
  upstream Crew wiring.
- Arbitrary native-memory formats, selective or streamed migration beyond the
  bounded complete namespace, and automatic multi-branch semantic merge.
- Arbitrary edited multi-hop causal ancestry: lack of sufficient baseline
  evidence produces a conflict.
- Historical body, vector, recipe, lease, credential, tool permission and
  execution-checkpoint transfer.
- Automatic artifact/environment verification; the receiving host must perform
  these checks.
- Semantic fidelity, real longitudinal task non-degradation, or superiority
  over a strong native-memory baseline.
- Secure physical erasure, Byzantine same-user processes, unreported remote
  withdrawals, power-loss behavior and Linux execution in this local run.

These are original engineering tests and independent synthetic rechecks. They
are not E3 paper reproduction or E4 target-host acceptance. The
[paper review](long-term-fidelity.md) is E2 method review; longitudinal semantic
experiments remain proposals.
