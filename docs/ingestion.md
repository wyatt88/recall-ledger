# Candidate intake and stable facts

Candidate intake separates capturing a message from accepting a memory record.
Every origin starts in `pending`, including `user_explicit` and
`tool_observation`. Origin is caller-supplied provenance, not proof that text is
true or authorization to accept it. A trusted caller must explicitly resolve
each candidate; this API does not authenticate a human reviewer or call a model.

## API

| Method | Parameters | Result |
| --- | --- | --- |
| `candidate_submit` | Required `event_id`, `body`, `origin`, `source`, `idempotency_key`; optional `fact_key`, `kind`, `volatility`, `valid_from`, `valid_until` | Body-free receipt identifying a pending candidate |
| `candidate_list` | Optional `state`, `limit` | Metadata only; default limit 50, maximum 100, with `total` and `truncated` |
| `candidate_inspect` | `candidate_id` | One candidate, with body visibility rules below |
| `candidate_resolve` | `candidate_id`, `decision` (`accept` or `reject`), `idempotency_key`; `expected_revision` when accepting an existing fact | Body-free resolution receipt |
| `fact_get` | `fact_key` | Current valid record in the ordinary `inspect` shape, plus `fact_key` and `record_id` |

Allowed origins are `user_explicit`, `user_message`, `tool_observation`,
`model_inference` and `import`. In particular, a host hook receiving arbitrary
user text should use `user_message`; it must not call every prompt an explicit
fact. Pending candidates are absent from `recall`, vector indexing, summary
discovery and the record snapshot export.

States are `pending`, `accepted`, `rejected` and `withdrawn`. Rejection clears
the stored proposal body. It cannot subsequently be changed to acceptance.
Retry a resolution with its original arguments and idempotency key to recover
the original receipt; another key does not reopen a terminal candidate.

Candidate receipts contain `candidate_id`, `event_id`, `state`, `record_id`,
`accepted_revision` and `revision` (the last three are null before acceptance).
The store adds its ordinary `commit_token` and `durable` fields. A receipt is
historical metadata and never grants current body access.

## Exact identity and conditional acceptance

`fact_key` is an object with exactly four nonempty string fields:

```json
{
  "entity": "project:example",
  "attribute": "implementation_language",
  "scope": "repository:example",
  "conditions": "default build"
}
```

Each field is limited to 256 UTF-8 bytes, rejects control characters and must
contain something other than whitespace. Identity is the exact four-tuple
within a namespace. Object field order is irrelevant. Case, Unicode spelling
and whitespace inside a nonempty string remain significant. There is no fuzzy
entity resolution, delimiter concatenation, pronoun resolution, negation
inference or implicit interpretation of dates in the body.

Accepting the first candidate for a key uses the existing `remember` path and
creates one stable record binding. Accepting a later candidate for that key
requires the record's current `expected_revision` and calls the existing
`revise` path. This preserves descendant invalidation, vector invalidation and
managed-summary repair fencing. The binding and resolution commit in the same
transaction as the record change and operation receipt.

Missing or mismatched `expected_revision` returns `revision_conflict` and keeps
the candidate pending. Supplying a revision for a new or unkeyed fact also
returns `revision_conflict`. A caller can read `fact_get.revision` before
attempting the update; another writer may still win, in which case read again
and review the pending proposal before retrying.

Without a key, each separately accepted event creates its own record. Equal
body text is not enough to establish a shared fact identity.

### Metadata on updates

For new records, defaults match `remember`: `kind="assertion"`,
`volatility="normal"` and no validity bounds. For an existing fact, omitted
`kind`, `volatility`, `valid_from` and `valid_until` inherit its current values.
An explicitly supplied value must match the existing value, including an
explicit `null` time bound. Otherwise acceptance returns
`fact_metadata_conflict` and leaves the candidate pending.

The existing `revise` operation changes body text, not those metadata fields.
Use a deliberately distinct key or conditions to represent a different
validity interval or fact type. A record that is already expired or otherwise
invalid is not revived by candidate acceptance; ordinary `revise` eligibility
checks still apply.

The candidate's required `source` describes that intake event. An update keeps
the canonical record's original `source`; it does not silently replace it.
Candidate inspection includes submitted metadata and `metadata_fields`, which
identifies which optional values were explicit. Accepted candidates also expose
`current_record_metadata` so callers can distinguish that intake metadata from
the current record's effective metadata.

## Event deduplication

`event_id` is unique within its namespace, independently of the operation
idempotency key. The same event and semantic payload returns the same candidate
after a restart, even if the caller uses a different operation key. Changed
payload returns `event_conflict`.

The event fingerprint covers body, origin, source, exact fact key, normalized
metadata and which metadata fields were provided. That presence mask matters:
omitted metadata can inherit an existing fact, while an explicitly supplied
default can conflict with it. The operation idempotency key is excluded from
this event fingerprint; ordinary operation idempotency remains enforced by the
store.

## Current reads and withdrawal

`candidate_list` never includes bodies. Use `candidate_inspect` to review one
proposal:

- A pending candidate returns its proposal with
  `body_source="candidate_proposal"`. It remains unaccepted retrieved data.
- Acceptance clears the candidate's stored proposal body. Subsequent inspection
  reads the current canonical record, with `body_source="current_record"`,
  `accepted_revision`, `current_revision` and
  `record_changed_since_acceptance`. It does not return the original accepted
  wording as if it were still current.
- An accepted record that is no longer valid has no body in the candidate
  response. `body_available=false` and `body_unavailable_reason` explain this.
- Rejected and withdrawn candidates expose metadata only.

All candidate responses mark text as `retrieved_data`. Neither an origin label
nor an acceptance decision turns body text into system instructions.

When `retract` erases a canonical record, its accepted candidates and all
known candidates with the same fact key become `withdrawn`, and their stored
proposal bodies are cleared. This includes pending and rejected candidates.
Unkeyed accepted candidates are also linked to their own record for withdrawal.
The erasure hook runs in the same transaction as source withdrawal; reads also
check the binding as defense in depth.

Fact bindings, event fingerprints and body-free receipts remain. A new event
targeting a withdrawn key returns `fact_retracted`; replaying an old event
cannot resurrect it. There is no implicit restore action. Any deliberate new
identity must be an explicit caller decision.

This is withdrawal of known links, not semantic matching of every possible
copy. An unkeyed pending message, another identity containing the same wording,
host conversation history and external files are not automatically discovered
or erased. Caller-supplied metadata (including event IDs, source labels and
fact keys) remains stored. WAL files, backups and external copies are outside
the logical body-erasure guarantee.

## Implementation and validation

Schema 3 adds `ingestion_candidates` and `fact_bindings`. Recovery must preserve
their identity tombstones and fingerprints together with records and operation
receipts. Rebuilding a database from current readable records alone would lose
event deduplication and withdrawn identities.

`tests/ingestion.rs` exercises pending intake for every origin, exact identity,
revision conflicts and a concurrent race, namespace isolation, restart event
deduplication, metadata inheritance, strict input validation, withdrawal,
temporal eligibility and existing vector/summary repair behavior. These are
deterministic engineering tests with synthetic text and vectors, not a
measurement of extraction quality, factual correctness or Agent task gains.
