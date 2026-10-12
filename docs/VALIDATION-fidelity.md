# Source-preservation development validation

Date: 2026-10-12, Asia/Shanghai. Base repository commit:
`ac39ce51b80e7067d43306509e3c7fb5fd55893f` (v0.4.0).
At the time of this validation, changes were local and uncommitted, on top of
the portable extension. No new release, registry upload, public deployment or
real-host configuration change was performed during that validation.
Rust/Python package version is still 0.4.0; schema is 4.

## Executed checks

Environment: macOS arm64; Rust 1.99.0 and minimum Rust 1.89.0;
Python 3.12.14; Node 26.10.0.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo test --locked --all-targets` | 213 passed, 0 failed, 0 ignored |
| `cargo +1.89.0 test --locked --all-targets` | Same 213 passed, 0 failed, 0 ignored |
| Full Python SDK/adapter/wheel suite | 116 passed, no skips |
| OpenCode Node fixtures | 8 passed, no skips |
| Two-daemon fidelity regression | 64 correction cycles, 73 exact-source checkpoints, passed |
| Existing migration/handoff example | A → B → A, correction and withdrawal passed |
| `cargo build --release --locked` | Passed |
| Release executable `examples/workflow.sh` | Current publication, late-result rejection and withdrawal passed |

New Rust coverage comprises 20 core fidelity tests, 8 runtime fidelity tests,
8 structured-metadata read/projection tests and two runtime contract additions
(shared worker budget and qualified input to the explicit model path).
These are included in the 213 total, not added again.
Three new Python tests use the actual model-free daemon and public SDK,
including automatic repair. A newly built ordinary Python wheel was tested
outside the source tree by the existing installation test.

The final stable debug executable used for Python, migration and 64-cycle
validation was copied before the MSRV build to avoid mixing binaries:
`a9d6ffcb0a47af86039785098fb0cda78a6a518de1a6bcba3e26936f73925315`
(SHA-256).
The release executable SHA-256 is
`023b33b9df815a3745640ee9527f4fac85e8e4c49e68c2da3b41b218c0d338da`.

## Independent review

A separate AI reviewer froze eight engineering scenarios before reading the
implementation author's new tests. Original fixtures contained negation,
exceptions, numbers, time/scope qualifications, Unicode normalization differences,
and a deliberately wrong intermediate paraphrase.

Initial independent execution covered all eight scenarios. The daemon case
first failed due to the review harness including an unsupported `jsonrpc` field
in the raw socket envelope; correcting that harness field made the unchanged
case pass. This was not an implementation fix or a removed assertion.

Five critical cases were rerun on the pre-metadata-fix stable build and then
again on the final executable above, passing on both:

- Fabricated/partial/wrong-recipe completion and wrong namespace/lease rejected
  by the Store, with subsequent valid completion still possible.
- Source revision and selected-intermediate withdrawal fences preserved.
- Imported or forged source labels did not inherit local verification.
- Budget failure left old output unavailable and allowed unrelated repair.
- A real daemon without model configuration automatically created/repaired
  evidence while an incompatible older model task stayed pending.

### Discovered counterexample and repair

After the first full suite passed, integration review identified that a source's
`fact_key` was stored separately from its body. Independent public-API probes
confirmed a blocking omission: conditions appearing only in that key disappeared
from capsule roots/body, repair, migration reconstruction, ordinary reads and
Markdown projections. Character and UTF-8 budgets also missed those fields.
The original suite had put its qualifications in source bodies and did not
detect this case. The negative results were retained; the tests were extended.

The fix preserves the complete entity/attribute/scope/conditions object in these
paths, includes qualifiers in budgets, and retains them as untrusted input for
explicit model summarization. Three independent metadata probes passed on the
final binary: evidence and migration (13 transitions), managed reads/projection
(4), and the UTF-8 budget boundary (4). Their tests first failed as expected on
the preserved pre-fix executable. The final critical F2–F6 rerun added 47
transitions. No models were used to determine these results.

The initial execution additionally tested a 16-generation capsule chain with
four source revisions, exact byte preservation and expiry. Those three scenarios
were not all rerun on the final binary. The independent review used 291 synthetic
state transitions cumulatively, within its 400-event allocation. The discovered
structured-condition omission is closed in the final reviewed paths.

## Reproduction

```sh
cargo test --locked --all-targets
cargo +1.89.0 test --locked --all-targets
cargo build --locked
PYTHONPATH=sdk/python RECALL_LEDGER_BIN=target/debug/recall-ledger \
  python3 -m unittest discover -s sdk/python/tests -v
PYTHONPATH=sdk/python RECALL_LEDGER_BIN=target/debug/recall-ledger \
  python3 examples/fidelity_regression.py --cycles 64
```

For the wheel installation test, also build the SDK wheel and set
`RECALL_LEDGER_WHEEL` to its absolute path. Otherwise that test is skipped.
The independent protocol, runnable probe, raw results, source hashes and full
logs are archived in the development workspace's
`research/recall-ledger-fidelity-2026-10-12/` directory.

## What these results support

These are original engineering tests of declared source preservation, recorded
dependency validity, budget refusal and managed repair. The direct-original
baseline received the same sources; it also preserved text. No performance or
quality advantage over that baseline is claimed.

No external model or paid service was called in this round. Local HTTP fixtures
in legacy provider tests are synthetic protocol responses. Neither those
responses nor repeated event cycles are semantic evals or years of real use.

Still unvalidated: source truth, undeclared influence, semantic consistency,
real-host task quality, long-term non-inferiority, competitive cost/quality,
large-scale capacity and Linux execution of this new working tree. CI now
includes the new regression runner, but hosted CI has not run on an unpushed
commit. This is not E3 paper reproduction or E4 real-host acceptance.
