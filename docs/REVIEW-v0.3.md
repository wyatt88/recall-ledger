# v0.3 independent engineering review

Review date: 2026-10-10. Reviewer: 魏慎言, the workspace's independent AI
review role. This is source review and local contract testing. It is not a
security certification, a paper reproduction, Claude host acceptance, or
evidence that RecallLedger improves task quality.

## Scope and conclusion

The review follows data across candidate admission, canonical revision,
derived output, vector indexing, context manifests, change notifications,
backup/restore, and managed Markdown. It also reads the Python Claude adapter's
installation, import, outbox, refresh, and uninstall paths.

Seven independently authored cross-module tests pass on the reviewed Rust
snapshot. No unresolved blocking contract issue was identified in the reviewed
cases. A Python import/outbox issue was reported to its implementation owner
and its repair rechecked below. This finding-limited conclusion
does not mean all concurrency, filesystem, host, or semantic failure modes
have been exercised.

## Independent counterexamples

The reviewer authored `tests/v03_adversarial.rs` separately from the
implementation owners' tests. All bodies, vectors, and processor outputs are
synthetic. No model is invoked.

| Counterexample | Observed result |
| --- | --- |
| A keyed correction arrives while a derived summary, a second-level manual job, a vector, and a previously compiled context depend on the old revision | An incorrect CAS leaves the record and event feed unchanged. Retrying the failed key with the correct CAS succeeds; the canonical ID remains stable, the old vector disappears, both context references fail validation, and the pending manual publication cannot commit. Candidate inspection resolves to the current canonical body. |
| A withdrawn fact has an accepted candidate and another pending candidate, then a backup is restored | Retraction and candidate tombstones survive the snapshot. Replaying the old event does not return its old body; a new event using the tombstoned fact identity cannot resurrect it. Old context/cursor history is rejected. |
| A backup contains leased summary work, then the restored worker reuses the original claim operation key | Restore issues a new history identity and clears old operation receipts. A new lease is required; completion with the original lease is fenced, while the new lease can publish. |
| Time passes beyond a record's expiry without any intervening mutation | The commit token is unchanged and there is no change event. `validate_context` and the live Markdown reader nevertheless reject the expired record. Explicit offline checksum inspection still sees the old snapshot; a refreshed projection excludes it. |
| The ordinary CLI reads an unchanged Markdown file after source withdrawal | `read-projection` fails with `projection_stale` and produces no body on stdout. Explicit `--offline-integrity-only` returns its weaker checksum guarantee. A restored database with identical source bytes cannot authorize a projection from the old history. |
| A human edits a generated Markdown file before source withdrawal and regeneration | Regeneration refuses the modified projection and preserves both the edit and the existing pointer. The store itself no longer recalls the withdrawn source. |
| A repeated host event ID changes the exact fact conditions | The event conflict cannot overwrite the other condition's record or emit a mutation event. The separately bound fact IDs and bodies remain intact. A context manifest cannot cross namespaces. |

Executed:

```sh
cargo test --locked --test v03_adversarial -- --nocapture
```

Result: **7 passed, 0 failed**, test execution 2.56 seconds on the final local
rerun after the recovery repairs. The expiry test waits for an actual wall-clock boundary; it does
not rewrite database timestamps to manufacture a result. This is not a
performance benchmark.

## Changes prompted or checked by the review

### Markdown integrity is insufficient for freshness

The original standalone projection reader checks the pointer, manifest, and
Markdown checksums. Correct checksums also describe a stale snapshot. A commit
token comparison alone would still miss expiry without a write.

The added `Store::read_current_projection` checks namespace and history,
requires the current commit token, recomputes the eligible record set at the
read time, checks revisions/body hashes, and compares the rendered Markdown
hash under one database snapshot. The default CLI uses this path. Offline
integrity inspection requires an explicit option and reports its weaker
guarantee. The reviewer exercised both paths independently.

This establishes freshness at the checked database snapshot. It does not lock
the store while an agent later acts, erase previously injected conversation
text, or prevent a caller from bypassing the API and reading a file directly.
The exact global commit check is conservative: an unrelated write can require
a projection refresh even when this namespace's returned content did not
change.

### Import can create a permanently rejected outbox entry

The reviewed import loop originally skipped all-blank files but divided each
remaining file into fixed-size chunks without skipping all-blank chunks. A
valid Markdown file with a long blank span could therefore enqueue an invalid
empty candidate. The daemon rejects such a body with `invalid_request`.
`flush_outbox` originally stopped on every daemon error and retained the entry,
so later flushes could repeatedly stop at the same permanently invalid request.

**Disposition:** repaired and source-reviewed. Blank chunks are skipped while
remaining chunk spans keep their original offsets. Known permanent candidate
errors preserve the complete original request and machine error code in private
dead-letter storage and allow the bounded batch to continue. Transport,
retryable, unknown-method, and schema errors preserve the queued request and
stop the batch. Repeated hooks/imports do not automatically requeue a quarantined
entry; explicit requeue keeps the original event ID, payload, and key.

The reviewer independently reran the owner's targeted tests for permanent-error
isolation, retry/schema-error preservation, both uninstall interruption points,
inactive old hook snapshots, and the whitespace import against an actual local
Rust daemon. **5 passed, 0 failed** in 0.606 seconds. These are reruns of the
owner's test designs, not five additional independently authored tests.

### Uninstall must resume after the settings write

The adapter now persists an uninstall intent containing the target bytes/hash
and fallback hash before changing settings. A retry accepts only the exact
installed state or recorded restoration target, checks the fallback, and
finishes the manifest transition. Source review and inspection of the owner's
before/after-write fault-injection tests found no additional issue in this
transition. Those owner-authored tests were included in the targeted rerun above.
An old host hook snapshot also checks current installation state under the
adapter lock and becomes inert during/after uninstall, before capture or recall.

The fallback remains the snapshot captured when uninstall was prepared. It is
not a live export on every retry, a full restorable backup, or reconstructed
native memory. Default uninstall keeps native automatic memory disabled.
Changing the settings independently causes a refusal instead of overwrite.

### Final recovery and cleanup diffs

An additional review reported three issues during integration. This reviewer
checked the resulting source changes and corresponding owner-authored tests:

- Backup/restore refuse a destination with an existing main file or SQLite
  WAL, SHM, or rollback-journal sibling, and recheck before publication.
  An unknown sidecar is preserved instead of silently removed.
- Snapshot validation compares normalized supported CREATE definitions, not
  just column lists. Missing foreign-key/unique constraints or changed index
  definitions fail closed. Some malformed SQLite schema fixtures fail inside
  SQLite before the explicit schema comparison; either rejection occurs
  before a destination is published.
- Cleanup renames a validated, now-unreferenced generation to `.gc-<uuid>`
  before deleting its files. An interrupted deletion therefore cannot appear
  to be an edited completed generation and permanently block refresh.
  Retired remnants remain unserved and are not securely erased.

The reviewer's seven cross-module tests were rerun after these changes. The
new sidecar/schema/cleanup cases are separate implementation-owner tests;
this reviewer did not independently recreate their original failing snapshots.
No additional feature scope was added by this final review.

## Material limits

- **Identity is declared, not inferred.** `fact_key` is an exact structured
  tuple. Different spellings, aliases, omitted keys, or wrong conditions can
  represent duplicates or unrelated facts. CAS protects the chosen identity;
  it does not establish that the chosen identity is semantically correct.
  The adapter submits bounded unkeyed message/import candidates and does not
  perform learned extraction or contradiction resolution.
- **Explicit resolution is not authenticated human approval.** Pending
  candidates are excluded until a resolution action, but another process
  with daemon access can resolve them. Provenance labels describe the declared
  source envelope and do not establish truth or endorsement.
- **Notifications are resynchronization hints.** Events contain no body and
  do not enumerate every affected descendant. Consumers must re-read current
  state. Time-based eligibility can change without an event. Event delivery
  is not a complete change-data-capture or push subscription protocol.
- **Retraction has a defined reach.** Canonical records, declared dependencies,
  indexes, and known keyed candidates are covered by the exercised paths.
  Unknown duplicate text, imported originals, adapter outbox files, exports,
  old backups, filesystem remnants, and already delivered host context are
  not erased. Generated MD edits are preserved even when this prevents
  cleanup; the resulting projection is refused, not silently rewritten.
- **Restore is point-in-time recovery.** A backup taken after withdrawal
  preserves that withdrawal. An older backup cannot know about later
  withdrawals. Rotating history identity prevents old cursors, manifests,
  and leases from masquerading as the restored history; it cannot recover
  changes absent from the backup.
- **A checked read does not authorize a future side effect.** The adapter
  refreshes on a submitted prompt and fails open on unavailability. It does
  not intercept every later action, remove old conversation text, or prove
  that the model obeys the retrieved-data boundary.
- **Local evidence remains bounded.** The review did not run a real Claude
  conversation, a paid model, a task-quality comparison, a long-duration load
  test, a power-loss test, or an adversarial same-user filesystem race. Shared
  directory locks and private permissions are engineering controls, not
  isolation from a hostile process running as the same OS user.

## Reviewed snapshot identifiers

SHA-256 after the final source-diff review. The Rust target was rerun following
the recovery repairs; the five selected Python targets were independently
rerun as described above. The owner/root owns the full release check. Later
edits require checking the relevant diff; these are file hashes rather than
a released commit.

```text
bb7098e1cf37b7c757f7e833d625401d79d39433f963b355eac8c014deed0e03  tests/v03_adversarial.rs
876e85cb1eb47d5a95f8f1a3705392d4ef4d900d39c8e5cc04c5695b55d526b5  src/ingestion.rs
9db93f29cd4dba3b1df2efd558ffc6f0bfae6bb79942cbe5022493f1b7c49307  src/changes.rs
f0d472be00e972dd84ab40d5f81668e565eb7c048b7e810f081a44853505690a  src/recovery.rs
6443dd0a7802234ca881e3950511440967e81f441ff6629a45a4fb67b8ce9ad8  src/projection.rs
729ccc558fba6e1d219820a2c1a8e4f5ba3ea5deb3424b267855f0dacc5c6cb0  src/lib.rs
dd845fc8d71d6a67325752a1fc0516f9f4f0b55dded9e493113e1e2a700a0fee  src/main.rs
76459eb42082cc962e491599918d105a570b3a2a3c3e5180897c135702afe7b9  src/mcp.rs
0910c4b2173b98e2d86235aba45c36a3cbc3cbf587b69a0c586bfcd86e99fd66  sdk/python/recall_ledger/claude_code.py
f04636bda72bf59a994f034bdc852c9fa83f0ef09e51366149cbb7577a330b9a  sdk/python/tests/test_claude_code.py
6844e3bbaae5830b2cd6d0915c349a85150b104c4b837c5a5242459c2c601652  docs/claude-code.md
```
