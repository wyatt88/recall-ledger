# Pre-release review

Review date: 2026-10-10. This is an independent AI-assisted review of the initial
implementation, not a security certification, formal verification, or claim of
production readiness.

## Scope and method

The review covered:

- `src/lib.rs`: namespace filtering, transactions, revision checks, dependency
  traversal, temporal validity, idempotency, and body-read paths.
- `src/transport.rs`: framing, daemon lifetime locks, socket permissions, response
  handling, and connection limits.
- `src/mcp.rs`: initialization, tool dispatch, request identifiers, and transport
  error boundaries.
- `sdk/python`: bounded transport, response validation, retry classification,
  and synthetic SDK tests.
- README files, architecture and security documentation, public examples,
  package metadata, CI configuration, and release candidate text.

Review combined source inspection, the independently authored core tests, and
targeted synthetic requests against a real local daemon and MCP adapter. No
private database, real conversation, model, or cloud service was used.

## Release finding: oversized request identifiers

**Initial finding — confirmed.** String request identifiers were accepted up to
the frame limit. A valid MCP `remember` frame of 1,048,331 bytes reached the
daemon and committed. The adapter's response included both text and structured
representations of the receipt, plus the original large identifier. The response
exceeded the 1 MiB limit, so the host received error `-32603` even though a later
`operation` lookup confirmed a committed mutation.

This was an error/success ambiguity at the public protocol boundary. Normal SDK
UUID identifiers did not trigger it, but the documented MCP surface allowed the
request.

**Repaired and independently checked.** String identifiers now have a 256 UTF-8
byte limit before dispatch at both the daemon and MCP boundaries. The original
oversized-identifier scenario and byte-boundary cases were rerun against the
repaired binary; no rejected request committed. Details appear below.

## Other findings addressed during implementation

- **Grouping baseline overlap:** initial `single` and `fixed` plans used the
  same code path. The owner replaced `single` with explicit `individual` groups;
  documentation distinguishes this from bounded `fixed` groups, first-token
  `topic` groups, and caller-labelled `volatility` groups.
- **Minimum Rust version:** the initial manifest declared 1.85 while locked
  `uuid` 1.27.0 requires 1.89. The manifest now declares 1.89. The independent
  contract run used Rust 1.99; a minimum-version build must be reported separately.

## Contract checks

The reviewed implementation makes the mutation and its receipt part of one
SQLite transaction. Publication checks captured revisions and recursively
checks recorded ancestry in the same transaction that inserts the output.
Changing a source invalidates dependent records; editing a derived record does
not discard its dependency edges.

Retraction marks the source and transitive descendants unavailable and clears
their managed bodies. Normal retrieval, explicit history, job reads, and
diagnostic export were inspected for alternate body paths. Receipts contain
identifiers and historical operation state rather than body text. Replaying an
old successful receipt does not reactivate a retracted record.

The core tests passed 20/20 on macOS arm64, including independent connections,
simultaneous revision attempts, mixed-input free text, ancestor expiry,
retraction after revisions, and reopening databases. The repeated-graph test
also requires an unaffected branch to stay usable. Details and test boundaries
are in [TESTING.md](TESTING.md).

The Python SDK tests were independently rerun: **7/7 passed**. They use a small
fake socket peer to check the client transport contract; this does not replace
the real daemon/MCP integration tests.

## Public-content review

Candidate text was searched for private filesystem paths, internal role names,
credential patterns, conversation dumps, and bundled research material. No such
content was identified in source or documentation. Matches on ordinary
`commit_token` variables were not credentials. Paper references are links and
method descriptions; no paper PDFs, model weights, or benchmark datasets were
present.

Python bytecode caches exist locally but are excluded by `.gitignore`, as are
build products, local databases, socket files, and environment files. The final
Git index was separately inspected: the 34 staged files at that review point
contained no binary caches, private path or credential-pattern matches, local
database files, or downloaded papers. New files added after that snapshot require
the same publication check; inspecting a working directory is not the same as
inspecting the actual files being pushed.

The dependency notice is a source-level overview. Any binary distribution must
include the applicable licence texts and notices for its resolved dependencies
and target platform; the overview alone is not an exhaustive binary manifest.

## Remaining limitations

- Frame size, connection count, body size, and ancestry traversal have limits;
  these are not a bound on total database size or query work. Current recall and
  export can scan all active records, and explicit history is not paginated.
  Large-store resource use needs separate measurement and subsequent limits.
- Read responses can exceed transport size, particularly large exports or MCP
  text plus structured output. Smaller reads or a trusted CLI export are needed;
  protocol framing alone does not make every allowed read fit in one response.
- Protect database and socket directories with OS permissions. This release
  does not defend against hostile code running under the same OS identity.
- No guarantee covers hidden processor inputs, semantic correctness, secure
  disk erasure, previously delivered context, arbitrary file rollback, or power
  failure and corruption outside the exercised scenarios.
- The bounded concurrent and graph scenarios are not exhaustive concurrency
  testing or a proof over all dependency graphs.

## Repair verification

The repaired daemon and MCP adapter were exercised through actual processes:

| Request | Observed result | Mutation committed? |
| --- | --- | --- |
| MCP string ID near 1 MiB | JSON-RPC `-32600` before dispatch | No; operation lookup returned no receipt |
| MCP string ID of 257 UTF-8 bytes | JSON-RPC `-32600` before dispatch | No |
| MCP string ID of exactly 256 UTF-8 bytes, using multibyte characters | Success | Yes |
| Direct daemon request with 257-byte ID | `invalid_request` | No |

An intermittent transport test also required a fixture correction. Closing a
socket immediately after writing does not establish that the server committed:
the result is uncertain. The lost-receipt test now keeps the original connection
open without reading its reply, confirms the committed receipt through a
separate connection, then closes the original connection. It still requires the
retried receipt to match and exactly one record to exist. Its wait bound now
exceeds the Store's five-second busy timeout.

The corrected test passed under Rust 1.89.0, then passed 32/32 repetitions using
four concurrent test processes. These repetitions target that fixture and are
not 32 additional independent product features or a full minimum-version test
matrix.

No remaining release blocker was identified within the reviewed initial-source
scope. The resource, trust, and deployment limits above remain applicable, and
the implementation owner must run the final combined checks after integration.
