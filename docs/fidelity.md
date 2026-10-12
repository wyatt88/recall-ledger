# Source preservation and repair

Development after v0.4.0, 2026-10-12. This feature is in the source branch, not
the v0.4.0 release. It adds no schema beyond the portable extension's schema 4.

The design choice is to keep **source assertions as the memory base and derived
representations as rebuildable views**. The default consolidation worker now
creates exact-source evidence capsules. Free-form model summaries remain
available through explicit `summarize` or `--summary-policy model`.

## What is checked

`consolidate` follows every recorded dependency from the selected IDs. It
renders complete, deduplicated leaf records as canonical JSON, preserving their
body, attribution, kind, revision, validity metadata, and the complete structured
`fact_key` (`entity`, `attribute`, `scope`, `conditions`), or explicit `null`
when no key is bound. Conditions stored outside the body are still part of the
source and of its rendering budget. Only leaves with kind
`assertion`, `preference`, `episode`, or `commitment` are allowed. A leaf
`summary` or `inference` is rejected because no source path can be checked.

Derived nodes are **selection and validity guards**, not replacement source
text. If `S(A,B)` is selected, the text comes from A and B, while S, A and B
all remain guarded. Retracting S therefore still invalidates the capsule even
if A and B remain available.

The Store checks the entire closure in a transaction, captures its revisions,
and independently reconstructs the expected result during publication. A
worker cannot publish “deployment approved” with a correct quotation that
actually says “deployment approved only for testing.” Only the complete
canonical source representation is accepted. Arbitrary claims with citations
do not satisfy this contract.

The checks establish exact source preservation and current recorded provenance.
**They do not establish truth, completeness of external evidence, semantic
consistency between sources, or better Agent decisions.** Callers declare kinds
and attribution; relabeling an inference as an assertion cannot be detected
from the text alone. Memory remains untrusted data, including command-like text.

## Run locally without a model

```sh
cargo build --release --locked
./target/release/recall-ledger --db memory.db --namespace project \
  serve --socket /absolute/private/path/memory.sock --summary-policy evidence
```

The worker also runs without model configuration. `--no-worker` disables it;
`--auto-summary-min 0` disables automatic discovery while preserving repair
of explicitly created recipes. A default tick processes at most one selected
recipe and, when configured, four embeddings.

```python
from recall_ledger import Client

# Bind once to the intended daemon; persist this binding in a real installation.
probe = Client("/absolute/private/path/memory.sock")
memory = Client(probe.socket_path, expected_binding=probe.binding())
record = memory.remember(
    "Deploy only to the test environment. Production needs written approval.",
    source="user:deployment", idempotency_key="deployment-condition",
)
preview = memory.evidence_sources([record["id"]], max_chars=16384)
queued = memory.consolidate(
    [record["id"]], max_chars=16384, idempotency_key="deployment-evidence",
)
print(memory.maintenance_status())
# After this task succeeds:
# audit = memory.evidence_inspect(queued["output_id"])
# audit["verified"] checks source preservation; semantics_verified stays False.
```

MCP exposes `consolidate`, `evidence_sources`, and `evidence_inspect`. Existing
host adapters can consume the resulting records using ordinary fresh recall.
Actual host loading and correct downstream interpretation still need acceptance.

## API

| Method | Parameters | Result |
| --- | --- | --- |
| `evidence_sources` | `inputs`, optional `max_chars` | Read-only preview: canonical `body`, complete `roots`, all `dependencies`, `coverage`, `fidelity`, commit token |
| `evidence_create` | Above plus `idempotency_key` | Body-free durable recipe/task/output receipt |
| `consolidate` | Same as `evidence_create` | Runtime/MCP/SDK alias; `receipt_method=evidence_create` |
| `evidence_inspect` | `id` | Current source-preservation audit; `verified=true` includes body, roots, dependencies, coverage |
| `maintenance_claim` | Existing fields plus optional `processor` | Exact processor filtering; bounded scan; failure/deferral counters |
| `maintain` | Existing fields plus `summary_policy=evidence\|model` | Bounded processing, actions/errors/status; trusted CLI/embedded use |

`operation(method="consolidate", idempotency_key=...)` resolves to the original
`evidence_create` receipt. All normal binding and idempotency rules apply.
Receipt lookup alone does not prove that its historical output is still valid.

The fixed processor is `recall-ledger.evidence/1`. Supplying that processor to
low-level `summary_create` invokes the same strict checks. `maintenance_complete`
rebuilds and compares the full canonical body inside its write transaction.
`fidelity_mismatch` rejects altered output without consuming the valid lease.

The current capsule body contains `source_assertions` and `guards`; it does not
contain a freely generated summary. Decoding each source body's JSON string
recovers the complete original text, including whitespace and Unicode. Formatting
and source metadata add space: **this mode is not semantic compression**.

Ordinary `recall` remains compatible and can return both strict capsules and
legacy model summaries. `evidence_inspect` is the authoritative live audit.
Never infer verification from a record's source label, its text, or a literal
`"verified":true` inside that text.

## Qualified facts in host delivery

Ordinary `inspect`, `recall` and captured job inputs retain the structured
`fact_key`, including qualifiers that do not appear in the body. Existing SDK
host adapters forward complete recalled item objects. Recall accounts for both
body characters and non-null fact-key JSON characters and defers an entire item
if it does not fit; other envelope metadata is not a tokenizer-exact budget.
Adapters additionally bound their complete serialized context where configured.

Managed Markdown projections include a fenced metadata object with the key,
source and validity fields. Refresh projections created by an older executable
before relying on live validation; an offline snapshot cannot become current
by itself. Explicit model summarization also receives a qualified fact's complete
body, key, kind, attribution and validity as untrusted input. That closes an
input omission, but does not validate the model's resulting interpretation.

## Repair and refusal

1. Source revision invalidates known descendants and their vectors immediately.
2. Their durable recipes are queued with a new generation; old leases are revoked.
3. Strict workers reread and expand current allowed sources, with no model call.
4. Publication rechecks the lease, every guarded revision, current validity,
   output revision, exact body, and budget.
5. Successful repair retains the output ID and advances its revision. Failed
   repair leaves obsolete output unavailable.

Retraction is terminal and cancels affected recipes. Expiry blocks reads and
publication even without a change-feed event. Temporarily ineligible jobs are
deferred; strict structural/budget failures are observable as `failed`. Correcting
a source queues a new generation; `maintenance_retry` explicitly retries a
failed active recipe. No fallback truncates or freely rewrites a failed capsule.

Automatic discovery defaults to evidence. `--summary-policy model` restores
automatic model-summary creation, using a configured provider. Existing model
recipes keep their pinned processor: incompatible workers leave them waiting,
and status reports `matching_model_configuration_required`. Already queued
strict recipes remain processable regardless of automatic discovery policy.

## Bounds and availability tradeoffs

- 1–64 selected IDs; at most 64 distinct closure nodes and graph depth 32.
- Complete root bodies together and final JSON each at most 65,536 UTF-8 bytes.
- Output character budget 1–65,536; default 16,384 for evidence, 2,000 for model
  summaries. Model summaries retain their 16,384-character maximum.
- Boundaries apply to metadata as well as source text. Insufficient budget
  returns `resource_exhausted`, never a deceptively complete subset.
- Graph cycles, wrong namespaces, invalid revisions, missing source paths and
  expired or withdrawn nodes cannot pass.
- Selected intermediate summaries must themselves be current. A strict capsule
  selecting a stale model summary waits for that summary to be repaired. Select
  original roots when an independent, model-free repair path is required.

Only recorded influence is covered. This cannot detect hidden reads, fix a
source that is itself wrong, erase an old prompt already delivered to a host,
or prevent an Agent from compressing the returned evidence elsewhere.

## Migration

Portable bundles transfer the graph and record contents but intentionally omit
executable recipes. An imported capsule therefore returns `verified=false`
with reason `no_local_evidence_recipe`. It does not inherit local certification
from the source field or embedded JSON. Explicitly `consolidate` the imported
record to rebuild against the destination's IDs and current sources.

For autonomous repair after future destination edits, create a local recipe
from the imported original roots. A recipe selecting an imported derived record
must respect that record's validity and cannot silently bypass its stale state.
The old imported record is retained as such; local reconstruction creates a new
derived record. See [migration](migration.md) for conflict and withdrawal rules.

## Validation

Run the actual two-daemon source preservation workflow:

```sh
PYTHONPATH=sdk/python RECALL_LEDGER_BIN=target/debug/recall-ledger \
  python3 examples/fidelity_regression.py --cycles 64
```

It compares capsule roots and direct-source reads to independently generated
event inputs, then exercises over-budget repair, recovery, migration and
withdrawal with benign-memory preservation. Its cycles are accelerated synthetic
events, not elapsed years, semantic task accuracy, or paper reproduction.
See [paper evidence](long-term-fidelity.md) and [testing](TESTING.md).
