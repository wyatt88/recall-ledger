# v0.4 local integration validation

Executed 2026-10-10 on macOS arm64. This is original engineering validation
with synthetic data and temporary projects, not a paper reproduction, a
behavioral benchmark, or E4 acceptance in a running Agent App.

## Final local checks

| Check | Observed result |
| --- | --- |
| Rust 1.99.0, `cargo test --locked --all-targets` | 147 passed, none failed/ignored |
| Minimum Rust 1.89.0, same command | Same 147 passed, none failed/ignored |
| Python 3.12.14, entire SDK suite with daemon and wheel configured | 95 passed, none skipped |
| Python 3.9.6, SDK/Claude/shared runtime/Crew/OpenCode subset | 86 passed, one wheel test intentionally skipped; Codex installer requires 3.11+ |
| Node 26.10.0, OpenCode protocol fixtures | 8 passed, none skipped |
| Formatting, Clippy with `-D warnings`, diff whitespace | Passed |
| Release build, synthetic workflow and grouping example | Passed |
| Isolated ordinary wheel install outside checkout | Passed; packaged JS runtime performs real bridge calls |
| Installed CLI entrypoints | Claude, ChatGPT, Codex, KiroCrew and OpenCode `--help` all passed |

The Rust total includes 8 new daemon/store binding tests and 6 new real stdio
MCP tests. The two Rust toolchains run the same scenarios and are not 294
independent tests. Targeted implementation and review reruns are subsets, not
additional unique cases. Python includes the actual-daemon tests; do not add
those again to its total. Node fixtures model the supported host protocol;
they do not launch OpenCode itself.

## Reproduce

Build a wheel and run the full Python suite so its wheel test is not skipped:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo +1.89.0 test --locked --all-targets
python3 -m pip wheel --no-deps --wheel-dir /tmp/recall-ledger-wheel ./sdk/python
PYTHONPATH=sdk/python \
RECALL_LEDGER_BIN="$PWD/target/debug/recall-ledger" \
RECALL_LEDGER_WHEEL=/tmp/recall-ledger-wheel/recall_ledger-0.4.0-py3-none-any.whl \
  python3 -m unittest discover -s sdk/python/tests -v
node --test integrations/opencode/tests/plugin.test.mjs
cargo build --release --locked
bash examples/workflow.sh
cargo run --locked --example partition_probe
```

Use Python 3.11+ for the full suite. The wheel test creates a fresh environment,
installs using `--no-index --no-deps`, runs outside the checkout, then installs
and executes the actual JavaScript → installed Python → Rust daemon path.
The wheel includes the runtime; no source-relative JavaScript dependency is
needed. GitHub CI runs Ubuntu/macOS and the minimum Rust toolchain; inspect the
run for the exact source commit before reporting its remote status.

## Covered failure paths

- A socket reused by another namespace or database refuses reads, old receipts
  and writes under the persisted binding. A normal restart preserves identity.
- A restored/new history rejects old work; binding remains checked after model
  work and on every request of a persistent connection.
- stdio MCP with a saved binding rejects destination changes across process
  restarts. Without a file, its current process pins startup identity.
- Real local adapters capture pending proposals, explicitly admit them, refresh
  after revision/retraction, and deliver queued events after an outage.
- User/assistant callback retries preserve event IDs; conflicting payloads do
  not silently replace the prior event. Manifests are separate per session.
- Privacy policy precedes capture/queue writes; Temporary returns no memory,
  and shared runtime Incognito does not persist read receipts.
- Hook packets are bounded by serialized bytes, and failures never return old
  recalled bodies. OpenCode preserves the host's system-array reference.
- Interrupted configuration changes recover from journals. An edit between
  patch generation and the initial write is refused before writing any target.
- An injected real SDK client must carry the installed binding, closing a
  preflight-only check that could otherwise permit misrouting.

## Review corrections and limits

Independent review found a configuration race: the initial installer could
build its patch from one snapshot and later accept a different snapshot as its
old state. It now requires the original bytes and checks every target under
the adapter lock before journaling/writing. Three new race/contract tests were
independently rerun. Packaging review found an absent JavaScript wheel asset;
ordinary installed-wheel execution now covers that path. See [review](REVIEW-v0.4.md).

No user memory, real host configuration or paid model was used. The local
ChatGPT/Codex integration requires supported local runtime hooks and explicit
host trust. KiroCrew needs trusted Gateway wiring and native authority/privacy
selection. OpenCode is pinned to V1 v1.18.35; V2 is unsupported. Actual App
sessions, warm continuations, compaction and platform-specific trust/loading
are still acceptance work. Settings generated by an installer are not proof
that the host used them. Core model-quality and native-memory comparisons
remain outside these tests.
