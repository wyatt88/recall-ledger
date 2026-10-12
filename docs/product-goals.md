# Product goals: reliable long-term memory and continuity across agent brands

Product direction clarified by the user on 2026-10-11. These are the project's
two intended outcomes. Acceptance designs below are proposals, not completed
benchmarks, a frozen protocol, or a claim of lossless migration.

## 1. Keep memory useful over long periods

The target failure is observable deterioration caused by the memory pipeline:
losing relevant evidence, dropping conditions during compression, confusing
scopes or dates, repeating corrected mistakes, or promoting unsupported
inferences into durable facts. Storing more records is not sufficient.

Treat three properties separately:

| Property | Required behavior | Current boundary |
| --- | --- | --- |
| Lifecycle consistency | Revisions, withdrawals and recorded dependencies control subsequent reads and publication | Implemented in managed paths; see [contract](CONTRACT.md) and [tests](TESTING.md) |
| Exact source preservation | Complete allowed original-source bodies and their recorded provenance remain recoverable from a derived view | Implemented in the post-v0.4 [strict evidence path](fidelity.md); incomplete budgets refuse, source truth remains unverified |
| Semantic fidelity | Claims preserve source meaning, uncertainty, conditions and disagreements across extraction and summaries | Candidate admission is explicit; automatic semantic normalization and fidelity verification are not implemented |
| Useful application | The host selects relevant, current evidence and correctly applies it to a task | Retrieval and adapters exist; real-host longitudinal quality remains unvalidated |

The engineering direction is to retain permitted source evidence separately
from derived interpretations, preserve provenance and applicability conditions,
and regenerate derived views from eligible sources when needed. Retention must
respect withdrawals; this is not a proposal to keep deleted bodies forever.
Repeated summaries must not silently replace their available evidence.

Future semantic processing should produce reviewable candidates, preserve
unresolved disagreement and record what was checked. A model's confidence or a
second model agreeing is not proof of truth. Freshness checks, semantic checks,
current-environment checks and permission to act remain separate.

### Longitudinal acceptance

Use independent project histories with stable facts, corrections, conditional
preferences, withdrawals and distracting events. At fixed checkpoints, increase
history length, compaction count and derivation depth, and measure:

- Retention of sufficient, valid evidence and its conditions.
- Newly introduced unsupported claims and recurrence of corrected mistakes.
- End-to-end valid task success, useful coverage and unnecessary refusal.
- Recovery time after a correction, plus total processing and maintenance cost.

Run probes read-only or on disposable checkpoint branches. Their questions,
answers and grading feedback must not become additional memory for later probes.
Compare against strong native memory and rebuilding from currently eligible
original sources under the same budget. Freeze acceptable degradation and
statistical rules after development calibration, before looking at held-out data.

Correct evidence that was actually delivered but still led to failure requires
host, reader and tool diagnosis. It does not automatically establish either
memory failure or inadequate model intelligence.

## 2. Let knowledge survive a change of agent brand

The intended user experience is: change the agent, keep the useful knowledge,
and continue accumulating it. The local ledger is an authority for the selected
dynamic-memory scope only after an explicit host cutover; rules and execution
permissions remain controlled by the user and host.

Migration can mean several different operations:

| Operation | Current capability | Remaining work |
| --- | --- | --- |
| Switch supported hosts while keeping one ledger | Bound adapters can address the same namespace/history | Actual host loading, context use, correction and return-path acceptance; Crew still needs upstream wiring |
| Bring in a previous host's native memory | An explicitly selected Markdown directory can enter the Claude adapter as pending candidates with path/hash/chunk provenance | Native format discovery, entity/scope mapping, duplicate/conflict handling and review of missing provenance |
| Move a whole RecallLedger database | Consistent SQLite backup and new-history restore | Reconcile post-snapshot changes, separately handle adapter state, and deliberately establish new bindings |
| Exchange knowledge with another RecallLedger deployment | Versioned namespace-complete `bundle_export`, read-only `bundle_plan`, transactional `bundle_apply`, source-specific conflict checks and tombstones | Selective/streamed transfer, native format mappings and richer causal merge; v1 is bounded to 512 KiB/1,000 objects |
| Continue work in another host | `handoff` prepare/inspect/verify/rebase with task data, references, artifacts and environment checklist | Actual host task acceptance and environment/artifact checking; execution checkpoints remain host-specific |

The current `export` response has `restore_supported: false`. A Markdown view
also lacks the semantics needed for a full round trip. Neither should be
advertised as the implemented `recall-ledger.portable/1` format. See
[migration and handoff](migration.md), [recovery](recovery.md) and
[native-memory ownership](native-memory.md).

### Implemented portable contract and remaining extensions

The current complete-namespace bundle carries:

- Format version, originating store identity, stable record/source identity
  mappings, and content hashes.
- Record kinds, scope, conditions, relevant time semantics, provenance and
  evidence status; unknown native metadata stays unknown.
- Revision/dependency relationships and admission/withdrawal state. A pending
  candidate must not become accepted merely because it was transferred.
- Withdrawal markers and reconciliation against newer changes, so an old
  snapshot or repeated import cannot revive removed information.
- Source-to-target mapping, explicit conflict reports, idempotent import,
  a dry-run report, exact destination commit checks and backup-based rollback.

V1 transfers current accepted content and candidate/withdrawal state. It omits
historical bodies, stale/retracted bodies, vectors and repair recipes. Unsupported
or oversized inputs fail without truncation; there is no general semantic merge.
Conflicting branches require reconciliation even when one has a larger revision.
Task references are mapped only through a currently applied source snapshot and
rechecked against the destination's current state. [The workflow](migration.md)
states the exact scope and multi-hop limitations.

Vectors and summaries are derived assets. Reuse requires compatible model and
schema identities; rebuilding them and its cost must be explicit. A hash proves
byte identity, not truth or authorization. Host credentials, tool permissions,
model weights, private reasoning state and pending tool calls are not knowledge
that this format can automatically transfer.

Changing database history intentionally invalidates old bindings and tokens.
Migration must not “fix” that by automatically replaying old outboxes into a new
store. The current installer is not a general force-rebind recovery tool.

### Migration acceptance

Use a source host A and destination host B. After the cutover, keep B's model,
prompt, tools, context budget and subsequent tasks fixed in four conditions:

1. **A → B:** A handles the earlier history, then B receives the migrated memory.
2. **B continuous:** B receives the same earlier history from the beginning with
   the same memory configuration.
3. **B empty:** B receives no prior long-term memory.
4. **B with sufficient evidence:** B receives current, legal source evidence for
   diagnosis only, not the answer. Exclude this condition from the main ranking
   and do not write its output back into memory.

For a controlled comparison, replay the same prior transcript and source events,
including assistant messages; otherwise different histories create another
cause of failure. Natural closed-loop histories may be compared separately as
complete systems. Keep B's strong native-memory configuration as another baseline.

The primary comparison is destination success in **A → B versus B continuous**,
paired by project history. Comparing A's old score to B's new score confounds
migration with model and host differences. Also measure time to useful service,
extra processing, manual re-explanation and source/scope preservation.

Test both switching clients on a shared ledger and a separate export/import
deployment. Then run B's corrections and withdrawals back through A → B → A.
Check pending-state preservation, no withdrawal resurrection, and current reads
on each host. Data preservation and behavioral continuity are separate results.
Target tolerances remain to be frozen; no zero-loss guarantee is established.

## Next engineering deliveries

1. A reproducible longitudinal regression harness and a real two-host
   A → B → A acceptance trace, using synthetic project data first.
2. Evidence-backed semantic ingestion/fidelity checks and actual environment
   validation by a receiving host. The handoff checklist alone does not perform it.
3. Native-memory import mappings, larger streamed bundles and reviewed merge
   decisions, extending the implemented bounded bundle/import workflow.

Each delivery must declare supported formats, host versions and failure paths.
Brand count, MCP connectivity, a successful import count or a stored SQLite
file is not sufficient acceptance. The two product goals must pass independently.
See [v0.4 validation](VALIDATION-v0.4.md) for what has actually been checked and
[the roadmap](roadmap.md) for strong baselines and evidence gates.
