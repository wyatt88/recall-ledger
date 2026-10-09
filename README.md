# RecallLedger

Local memory infrastructure for agents, written in Rust.

RecallLedger stores versioned records and tracks the inputs used to derive new
records. When a source changes, its dependent results stop being readable. A
worker that finishes late must validate its inputs before it can publish.

[中文说明](README.zh-CN.md) · [Architecture](docs/architecture.md) ·
[API contract](docs/IMPLEMENTATION-CONTRACT.md) · [Research](docs/research.md) ·
[Contributing](CONTRIBUTING.md)

## Why this exists

Suppose an agent remembers “the project language is undecided” and starts building
a summary. You then choose Rust. Updating the original record is not enough:
the old summary and the worker still processing the old input must be handled too.

RecallLedger makes this a storage contract:

```text
remember → prepare inputs at known revisions → process externally → publish
                 │                                                  │
                 └─ source changes → withdraw dependents ────────────┘
                                      reject stale publication
```

This can back an agent framework's existing long-term memory interface. It does
not replace the framework's execution checkpoints, model context, or model weights.
An external process does not inherently improve an agent's reasoning.

## Current scope

The 0.1 implementation is an early, single-machine foundation:

| Available surface | Behavior |
| --- | --- |
| Rust library and CLI | SQLite records, revisions, explicit namespaces, validity windows |
| Derivation protocol | `prepare` captures inputs; `publish` validates all recorded dependencies |
| Revision and retraction | Transitive withdrawal of dependent records; guarded history and job reads |
| Retry handling | Durable idempotency receipts and receipt lookup |
| Recall | Local literal-token text search, CJK fallback, explicit character budget |
| Local daemon | Unix-domain socket on macOS/Linux; no HTTP listener |
| MCP adapter | Stdio tools for remember, recall, inspect, revise, and retract |
| Python SDK | Client for the local daemon |
| Experimental grouping | Deterministic grouping rules for later consolidation experiments |

No model, API key, embedding download, or cloud account is required. The core
does not generate summaries: a trusted external processor supplies the proposed
body. Vector retrieval, RRF, automatic repair scheduling, repository-state
validation, learned memory selection, and benchmark results are future work.

## Build and try it

Prerequisites: Rust 1.89 or newer with Cargo and a C compiler for bundled
SQLite. The demo also uses Python 3. The daemon and demo target macOS/Linux.

From a checkout:

```sh
git clone https://github.com/wyatt88/recall-ledger.git
cd recall-ledger
cargo build --release --locked
./target/release/recall-ledger --help
bash examples/workflow.sh
```

Install the tagged source release:

```sh
cargo install --git https://github.com/wyatt88/recall-ledger.git --tag v0.1.0 --locked
```

The package has not been published to crates.io or PyPI. The Python SDK ships in
this repository. Release 0.1.0 distributes source; build for your local platform.

The demo uses a fresh temporary directory and synthetic records. It checks that
a summary prepared before a source revision cannot be published afterward, then
publishes a fresh summary and retracts its source.

For an interactive database:

```sh
mkdir -p .local-state
chmod 700 .local-state

./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  call remember --params \
  '{"body":"The project uses Rust.","kind":"commitment","idempotency_key":"decision-1"}'

./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  call recall --params '{"query":"Rust","max_chars":1200}'
```

Each mutating request needs a nonempty `idempotency_key`. Reuse that key with
identical arguments when retrying an uncertain result. Reusing it with different
arguments returns a conflict. Use `operation` to retrieve the original receipt.

## Run beside an agent

Start one daemon for a database in a private local directory:

```sh
./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  serve --socket .local-state/memory.sock
```

The namespace is selected by trusted transport configuration. Requests cannot
switch it by adding a namespace field to their parameters.

For an MCP-capable host, configure this stdio command using absolute paths:

```json
{
  "mcpServers": {
    "recall-ledger": {
      "command": "/absolute/path/to/recall-ledger",
      "args": ["mcp", "--socket", "/absolute/path/to/memory.sock"]
    }
  }
}
```

Start the daemon before the MCP adapter. The adapter exposes five record tools;
the full derivation protocol is available through the daemon, CLI, and Rust
library. Host-specific MCP configuration and approval behavior remain the host's
responsibility. The adapter targets MCP `2025-11-25` and provides tools only,
without sampling or task capabilities.

The Python SDK has no third-party runtime dependencies:

```sh
PYTHONPATH=sdk/python python3 examples/python_agent.py \
  --socket .local-state/memory.sock
```

```python
from recall_ledger import Client

memory = Client("/absolute/path/to/memory.sock")
receipt = memory.remember(
    "The project uses Rust.",
    kind="commitment",
    idempotency_key="decision-2",
)
result = memory.recall("Rust", max_chars=1200)
```

The SDK does not retry automatically. If `TransportError.outcome_unknown` is
`true`, recover with `operation` and the original method/key, or retry identical
arguments with that key.

Use one authoritative long-term store. If a host independently copies a record
into its own profile or context, RecallLedger cannot retract that copy.

## Guarantees and limits

- Mutation and its idempotency receipt commit in the same SQLite transaction.
- A derived record is published only after its captured revisions, ancestry, and
  validity conditions have been checked in that transaction.
- Revising or retracting a source withdraws its transitive dependent results.
  Ordinary recall returns only currently readable records.
- Retraction blocks subsequent managed reads and clears managed body content.
  It is **not secure erasure of SQLite pages, WAL files, backups, or copies already
  returned to another process**.
- A dependency means “this processor received this input,” not “this input proves
  every sentence true.” Hidden model context, unreported tool reads, and model
  training data are outside this contract.
- Namespaces are an application boundary, not a sandbox against hostile code
  running as the same operating-system user. CLI and library access are trusted.
- `export` is a diagnostic snapshot, not a backup or restore format. Restoring or
  cloning database files is outside the 0.1 commit-token contract.
- Validity timestamps must be supplied correctly. The store cannot discover an
  unreported real-world change.

See [the architecture](docs/architecture.md) and [security policy](SECURITY.md)
for the boundaries in detail.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

CI runs the Rust checks on Ubuntu and macOS. A green test run establishes only
the cases covered by that run. It does not establish production readiness,
superior agent task performance, or successful paper reproduction.

## Research basis

The design draws on work about tiered context, typed memory, dependency repair,
state-aware reuse, and memory evaluation. [Research notes](docs/research.md)
identify six fixed paper versions, distinguish implemented mechanisms from
future work, and describe a testable grouping hypothesis.

The project is an original engineering implementation. It is not an official
implementation of those papers, and it does not claim their reported scores.

## License

Apache-2.0. See [LICENSE](LICENSE), [NOTICE](NOTICE), and
[third-party notices](THIRD_PARTY_NOTICES.md).
