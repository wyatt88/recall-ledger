# Third-party notices

RecallLedger's original source is licensed under Apache-2.0. Dependencies retain
their original licenses. The `LICENSE` file is the unmodified Apache License,
Version 2.0 text from <https://www.apache.org/licenses/LICENSE-2.0.txt>.

## Direct Rust dependencies

Exact versions are recorded in `Cargo.lock`; this table identifies upstream
projects, not a replacement for the notices in their source distributions.

| Dependency | Purpose | Upstream license |
| --- | --- | --- |
| [clap](https://github.com/clap-rs/clap) | Command-line parsing | MIT OR Apache-2.0 |
| [fs2](https://github.com/danburkert/fs2-rs) | File locking | MIT OR Apache-2.0 |
| [rusqlite](https://github.com/rusqlite/rusqlite) | SQLite bindings | MIT |
| [serde](https://github.com/serde-rs/serde) | Serialization | MIT OR Apache-2.0 |
| [serde_json](https://github.com/serde-rs/json) | JSON encoding | MIT OR Apache-2.0 |
| [sha2](https://github.com/RustCrypto/hashes) | SHA-2 hashing | MIT OR Apache-2.0 |
| [uuid](https://github.com/uuid-rs/uuid) | Storage and record identifiers | MIT OR Apache-2.0 |
| [tempfile](https://github.com/Stebalien/tempfile) | Test directories | MIT OR Apache-2.0 |

`rusqlite`'s `bundled` feature compiles SQLite through `libsqlite3-sys`. SQLite
is in the [public domain](https://www.sqlite.org/copyright.html).

This is a direct-dependency overview, not an exhaustive binary license manifest.
Transitive crates, build tools, and platform-specific dependencies are resolved
in `Cargo.lock`. A binary distribution must include the applicable license
texts and notices for its resolved dependencies and target platform.

## Research references

[Research references](docs/research.md) are links and factual descriptions.
No paper PDFs, benchmark datasets, third-party memory implementation, or model
weights are bundled in this project. Using an external model or dataset may
introduce separate license obligations.
