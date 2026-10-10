# v0.2 validation

Recorded 2026-10-10. These are original engineering checks, not paper
reproductions, a security certification, or proof of Agent task improvement.

## Automated coverage

The release test suite contains 88 Rust tests:

| Suite | Tests | Coverage |
| --- | ---: | --- |
| Core contracts | 18 | Revisions, idempotency, namespace, validity, recorded lineage |
| Mutation sequences | 2 | Restart replay and repeated dependency graphs |
| Planning | 4 | Group boundaries, budgets, constructed cost counterexamples |
| Vector/migration | 12 | Cosine/RRF, model spaces, invalidation, actual v0.1 DDL migration |
| Managed maintenance | 14 | Leases, restart, output CAS, multilevel repair, withdrawal |
| Local providers | 13 | Real HTTP to stubs, bounds, timeout, redirects, malformed responses |
| Configured runtime | 10 | Indexing, summaries, repair, in-flight source changes, backoff |
| Daemon transport | 12 | Actual processes and sockets, worker automation, disabled worker |
| MCP unit tests | 3 | Negotiation, protocol errors, machine-readable tool failures |

The Python SDK has 11 tests. Formatting, Clippy with warnings denied, the release
build, the original synthetic workflow, and the grouping mechanism example are
also checked. CI tests stable Rust on Ubuntu/macOS and Rust 1.89 on Ubuntu. See
the exact release commit's [CI results](https://github.com/wyatt88/recall-ledger/actions/workflows/ci.yml);
a historical passing run does not validate a later commit.

Vector/migration and runtime tests were designed by an AI reviewer separately
from their implementation. Queue/provider tests were independently rerun by
that reviewer, but this does not make their test design independent. Details
and counterexamples are recorded in [the review](REVIEW-v0.2.md).

An initial local Rust 1.89 test run exposed a race in the synthetic HTTP fixture:
macOS accepted sockets inherited a nonblocking listener's flag. The fixture now
explicitly restores blocking mode before reading. This was a test-server bug;
the product's socket listener is blocking. The failed run was retained in the
local validation record before rerunning the repaired suite.

## Real local model smoke

An isolated daemon and the Python SDK exercised an existing local
EmbeddingGemma 2 service and a `gemma4:31b-mlx` chat service, using only synthetic
project facts. No weights were downloaded, no paid/cloud inference was used,
and no real user memory was imported.

- Embedding weights revision:
  `914f7f89142e33e77833254d9c9b90c3cef7303b`, 128 dimensions.
- Served encoder signature:
  `0ea16f19627ece8078d464d0efc776527b40babba613412285f58080db0d1cce`.
- Chat model's locally reported digest:
  `a22a363052da770302019d5afab29bd967f9318e4377bddd297421189ea09d7d`.

These identifiers document the tested local services. They do not imply that
the generic HTTP protocol attests weights on another installation.

The integrated run used seven embedding requests and two generation requests,
under a predeclared cap. Two prior exploratory provider calls bring the total
to 11 requests. The integrated checks observed:

1. Two records automatically formed a summary; all three records were indexed.
2. A paraphrased vector query returned the SQLite source, ranked first.
3. Changing the source's launch month from April to August immediately made the
   previous summary unavailable.
4. The worker repaired the same output ID to revision 2 with the updated parent
   revision. The synthetic output retained Rust/SQLite and changed April to August.
5. Hybrid retrieval returned the repaired output after reindexing.
6. Withdrawing the source cancelled the recipe; ordinary inspection and lexical
   retrieval no longer returned the source or its summary.
7. Restarting the daemon preserved that terminal state and the unrelated source.

This is plumbing and lifecycle evidence. The database contained three records
and the vector query requested three results; lexical retrieval also found all
three. It is **not** a discriminating semantic-quality benchmark. No additional
vector query was issued after withdrawal; vector deletion and stale-read
protection are covered by the independent vector contract tests.

The reviewer inspected the implementation owner's real-run record and matching
binary hash, but did not independently rerun these real models. Generated text
was checked against these two synthetic inputs only.

## Unverified properties

Large-database latency, ANN indexing, grounded summarization quality, resilience
to adversarial memory text, host-specific Agent task gains, physical erasure,
and arbitrary backup/restore handling remain unverified or unimplemented.
These checks do not establish production readiness.
