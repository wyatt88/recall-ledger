# Contributing

Contributions are welcome. Start with the [implementation contract](docs/IMPLEMENTATION-CONTRACT.md)
and [architecture](docs/architecture.md).

## Development setup

Use stable Rust with `rustfmt` and `clippy`, a C compiler, and Python 3 for the
workflow demo. Keep the application lockfile committed.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --release --locked
bash examples/workflow.sh
```

Use synthetic records and temporary databases. Do not commit real conversations,
credentials, local databases, socket files, private filesystem paths, or model
artifacts.

## Changes to storage or protocol

Describe the observable behavior before and after your change. Include a focused
test for the failure mode when changing:

- Revision checks, ancestry, or visibility.
- Idempotency and retry behavior.
- Retraction, history, job reads, or export.
- Namespace binding, framing limits, or daemon locking.
- Schema migration or compatibility.

Make the state transition and its receipt atomic. Keep dependency validation in
the publication transaction. An index, cached body, error, or receipt must not
become a way to retrieve retracted text.

Schema and wire-format changes need explicit compatibility notes. Do not silently
reinterpret commit tokens or re-enable stale records.

## Research and performance changes

Keep deterministic correctness tests separate from model benchmarks. Document
the baseline, fixed input, model or processor version, total budget, metric,
and expected failure condition before interpreting results.

Include preprocessing, repeated reads, IPC, retries, and repair work in cost
comparisons. A faster microbenchmark does not establish improved agent outcomes.
Experimental grouping strategies must remain labeled experimental until they
have suitable evidence.

## Pull requests

Keep a pull request focused and include:

1. The problem and resulting behavior.
2. Relevant implementation choices and trade-offs.
3. Checks actually run and their outcomes.
4. Known limitations or follow-up work.

Contributions are accepted under the project's Apache-2.0 license. Only submit
material you have the right to contribute. Preserve third-party notices.

For a potential vulnerability, follow [SECURITY.md](SECURITY.md).
