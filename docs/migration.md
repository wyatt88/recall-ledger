# Portable memory and task handoff

Implemented in the source branch after v0.4.0. This is a local, bounded workflow
for RecallLedger namespaces and task briefs. It does not import every brand's
native format, transfer a running model session, or establish real-app acceptance.

There are two paths:

- **Same ledger, another agent:** keep the explicitly chosen namespace/history,
  prepare a handoff, and verify it with a separately bound receiving client.
- **Another ledger:** export, preview conflicts, apply the reviewed import,
  explicitly rebase the handoff through the imported identity mappings, then
  verify it at the destination.

The receiver still reviews the task, artifacts, environment and permission to
act. A successful evidence check does not authorize execution.

## Run the complete synthetic workflow

From the repository root, with Rust and Python 3.9+:

```sh
cargo build --locked
PYTHONPATH=sdk/python RECALL_LEDGER_BIN=target/debug/recall-ledger \
  python3 examples/migration_handoff.py
```

This creates two real daemons and disposable databases. A supplies a decision
and an unreviewed candidate; B imports them, receives the task, corrects the
decision, and returns it to A. Finally B withdraws it and A rejects the old
handoff. The example checks stable identity, revision progression and pending
state. It makes no model calls and does not launch ChatGPT, Codex, KiroCrew or
OpenCode.

## Portable format

`bundle_export` returns `recall-ledger.portable/1`. Export covers the **whole
selected namespace**, with a maximum of **1,000 records plus candidates and
512 KiB of compact UTF-8 JSON**. A larger namespace fails explicitly; no
partial bundle is returned. Selective export and streaming are future work.

| Included | Transfer semantics |
| --- | --- |
| Current records | Body, kind, source label, revision, validity interval, state and timestamps |
| Root identities | Origin history/namespace/ID survive multiple hops; source-local IDs are separate |
| Dependencies | Exact parent identities and captured revisions; missing parents and cycles fail |
| Fact keys | Exact entity/attribute/scope/conditions tuple; no semantic merge |
| Candidates | Pending stays pending; accepted, rejected and withdrawn states remain distinct |
| Tombstones | Retracted records carry no body and cannot be revived by importing an older active state |
| Integrity metadata | Format version, source binding, source sequence, payload digest and synchronization preconditions |

Stale and retracted record bodies and historical revision bodies are omitted.
Active records whose validity interval has expired still carry their interval;
normal reads at the destination enforce it. Accepted candidates refer to their
canonical records rather than carrying a second accepted body.

Vectors, model configuration, summary recipes, maintenance leases, operation
receipts, host outboxes, credentials, tool permissions, model weights and live
execution checkpoints are excluded. Imported valid summaries retain their
declared dependencies but **do not acquire an automatic repair recipe**.
Configure indexing and explicitly create destination summary jobs as needed.

The SHA-256 digest covers only the compact, sorted-key UTF-8 serialization of
`payload`, with no newline. The format is independently checked. It detects
changes; it does not authenticate a sender, verify a claim or grant permission.
Use a source identity obtained through a trusted local channel. Accepting a
bundle is a trusted administrative import of its existing admission states.

## Export, review, apply

The following commands use example paths. Save the source binding before
transfer; `call binding` reads it directly, or use `binding --socket` for a daemon.
The destination namespace can have a different name.

```sh
umask 077
recall-ledger --db source.db --namespace project-a call binding > source-binding.json
recall-ledger --db source.db --namespace project-a bundle export --output memory.json

recall-ledger --db target.db backup --destination target-before-import.db
recall-ledger --db target.db --namespace project-b bundle plan \
  --bundle memory.json --source-binding source-binding.json --output import-plan.json
```

Inspect `import-plan.json`: source and target bindings, reviewed digest,
`can_apply`, conflict reasons, counts, and source-to-target identity mappings.
Planning reads a consistent snapshot and makes no data or commit-sequence
changes. Opening a new destination creates its empty database/schema.

Once that specific plan is accepted:

```sh
recall-ledger --db target.db --namespace project-b bundle apply \
  --bundle memory.json --plan import-plan.json --idempotency-key migration-project-a-001
```

The write revalidates the bundle, destination identity, **exact target commit**
and conflicts in one transaction. Any concurrent commit, including an unrelated
namespace commit in that database, requires a fresh plan. No records, indexes,
origin mappings, watermarks or success receipts survive a failed transaction.
The CLI does not silently refresh the reviewed plan.

Output files are new private files (`0600` on Unix); existing files and symlink
inputs are refused. Keep bundles under the same privacy controls as the source
database. An old exported copy cannot be remotely erased by a later withdrawal.

After an uncertain response, retry the same arguments and idempotency key or
read its `operation` receipt. Do not generate a fresh key until the earlier
outcome has been resolved. Receipts and change events contain no record bodies.

### Conflicts are explicit

| Condition | Result |
| --- | --- |
| Wrong source or destination history/namespace | Binding failure |
| Tampering or different reviewed digest | Digest failure |
| Target changed since review | `target_changed`; inspect a new plan |
| Old source sequence, same-sequence different payload, or missing previously known tombstones | Import conflict |
| Same fact/event key belongs to a different root identity | Import conflict; no automatic deduplication |
| Both branches changed independently, even if one has a larger revision number | Import conflict; revision magnitude is not causal proof |
| Pending/accepted state would regress, or a withdrawn identity would become active | Import conflict |
| Unsupported schema, incomplete graph, cycle or oversized package | Rejected before applying |

Same-source progression uses the last imported state for that source. A return
update can also prove that its captured baseline equals the destination's
current state. When that proof is absent, the importer refuses to guess.
Root identity survives multiple hops; **edited A → B → C → A histories may
require reconciliation**, because v1 does not carry a full causal ancestry log.
There is no force-merge flag.

An identical already-imported snapshot can return a replay/no-op report; it
does not overwrite destination edits or resurrect withdrawals. Replaying the
same mutation key returns the original historical receipt, not current content.
Using a fresh key for that snapshot leaves memory objects unchanged but still
commits a new operation receipt and change event, advancing the commit sequence.

## Prepare and receive a task

Install the Python SDK to obtain `recall-ledger-handoff`, or run
`PYTHONPATH=sdk/python python3 -m recall_ledger.handoff`.

A task JSON has these exact required fields:

```json
{
  "task_id": "release-preparation",
  "goal": "Continue preparing the local release.",
  "progress": ["The architecture decision has been reviewed."],
  "open_questions": ["Which environment should receive the release?"],
  "artifacts": [{"uri": "workspace:README.md", "description": "Review the current file."}],
  "environment_checks": [{"condition": "Confirm repository, branch and local toolchain."}]
}
```

Artifact entries may include a lowercase SHA-256 `sha256`; the handoff utility
records it without opening the URI or checking the file. Conditions may include
a `reason`. Task progress is sender-supplied prose, not verified completion.

Obtain a fresh `manifest` from a RecallLedger `recall` result and save it as
`manifest.json`. The manifest is the **object under `result["manifest"]`**, not
the entire recall result. Save the previously selected daemon binding separately.

```sh
recall-ledger-handoff prepare \
  --socket source.sock --binding-file source-binding.json \
  --task-file task.json --manifest-file manifest.json --output handoff.json
recall-ledger-handoff inspect --file handoff.json
recall-ledger-handoff verify \
  --socket source.sock --binding-file source-binding.json --file handoff.json
```

`recall-ledger.handoff/1` is at most **64 KiB**, containing task data, up to 100
evidence IDs/revisions, a fixed ledger binding, preparation time and digest.
Inspect checks shape and integrity only. Prepare and verify check current
versions, status, declared dependencies and validity times through the daemon.
No citations is reported as `has_evidence: false`, even if the empty manifest
passes structural validation.

Verify exits 0 for current evidence, 2 for obsolete evidence, and 1 for malformed
input, binding or transport errors. A successful check is time-specific; check
again before subsequent use after waiting or receiving a change notification.
Neither verification nor a central withdrawal erases text already in a host
prompt.

## Rebase after a cross-ledger import

After applying `memory.json` at the destination, obtain and explicitly select
the target binding. The original handoff remains bound to the source.

```sh
recall-ledger binding --socket target.sock > target-binding.json
recall-ledger-handoff rebase \
  --socket target.sock --binding-file target-binding.json \
  --file handoff.json --bundle-file memory.json --output target-handoff.json
recall-ledger-handoff verify \
  --socket target.sock --binding-file target-binding.json --file target-handoff.json
```

`bundle_context` performs a read transaction against the destination: the bundle
must match that source's latest applied snapshot; every cited source-local ID
must match its exported revision and root identity; the corresponding target
state must still match the exported state and be readable. A receipt supplied
by a caller is not accepted as proof of an identity mapping.

The SDK checks the newly produced target manifest again before writing the new
handoff. Its `rebased_from` records the immediate predecessor's packet digest,
bundle digest, source binding and source manifest. It does not grow an unbounded
chain. A missing import, obsolete bundle, revision change, expiry or withdrawal
prevents rebasing.

Receiving hosts must separately:

1. Select the intended target ledger binding in their own supported setup flow.
2. Verify the handoff and read current evidence through that binding.
3. Check local artifacts, repository state, environment and task permissions.
4. Start fresh reasoning from the received task data and current evidence.
5. After later corrections, return a new bundle and handoff through the same
   preview/apply/rebase checks.

This utility does not modify installed adapter bindings, replay old outboxes,
disable native memory or resume private execution checkpoints automatically.
The existing adapter documentation still governs host cutover.

## Recovery and acceptance boundaries

Take a consistent backup before applying to an established destination.
Rollback uses [restore](recovery.md) into a **new path and history identity**;
re-select the destination and revalidate clients deliberately. There is no
selective undo of arbitrary concurrent post-import changes, and restoring an
older backup cannot recover corrections made after that backup.
Restore pins original record and candidate provenance before changing the
transport history. Restored old records therefore cannot evade a destination's
known tombstones simply by claiming the new database identity as their origin.

The reproducible example and tests validate synthetic database/SDK/CLI flows.
They do not establish semantic fidelity, real-brand behavioral continuity,
indefinite retention or improved task scores. See [long-term evidence](long-term-fidelity.md),
[product goals](product-goals.md), and the development validation record.
