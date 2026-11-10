# Change explanations and historical evidence

Implemented 2026-11-09. Schema 5 adds metadata-only record events, a read-only
`impact_preview`, and a paginated `timeline`. Existing mutations, revision
compare-and-swap, namespaces, binding checks, idempotency receipts and the
body-free `changes` feed keep their contracts.

This is an engineering implementation with synthetic local checks, not a
longitudinal quality result or a real-host acceptance claim. Evidence status:
code/method review **E2**, plus local tests executed. No paper replication (E3)
or target-scenario acceptance (E4) is implied.

## Interfaces

Both operations work through `Store.execute` / `execute_bound`, Runtime, the
Unix socket protocol, `recall-ledger call`, and MCP tools. They do not invoke a
model or write a record, receipt, event, or commit.

### `impact_preview`

Parameters:

```json
{"id":"record-id","expected_revision":2}
```

`id` is required; `expected_revision` is optional and, when supplied, must be a
positive integer equal to the current revision. A mismatch returns
`revision_conflict`. A withdrawn or foreign-namespace target returns
`not_found`. Unknown parameters fail.

The result contains:

- `record`: the current record and `applicable_now`. Ineligible derived text is
  omitted and has `body_withheld_reason`.
- `sources`: complete, currently eligible registered leaf assertions, with
  `id`, `revision`, `body`, `source`, and exact `fact_key`. An unavailable
  source has metadata and a withholding reason. An inference or summary with
  no source path is marked `source_evidence_missing`.
- `dependencies`: registered edges with `child_id`, `parent_id`,
  `captured_revision`, `current_revision`, and `applicable_now`.
- `revision_chain`: oldest-to-newest revision events, using the same body gates
  as `timeline`.
- `descendants`: all registered transitive descendants, containing only IDs,
  revisions, kinds, statuses and current eligibility.
- `effects.revise`: `allowed`, `invalidated_descendants`, and `blocked_reason`.
  Managed summaries must be regenerated from their sources.
- `effects.retract`: `allowed`, `withdrawn_descendants`,
  `physical_erasure:false`.
- `effects.prepared_jobs_affected`: distinct still-prepared jobs whose inputs
  include the target or a registered descendant.
- `preview_only:true`, `requires_revision_check_on_write:true`,
  `registered_dependencies_only:true`, `checked_at`, and `commit_token`.

Counts for invalidation/withdrawal include non-retracted registered descendants;
already stale descendants remain affected. Job counts describe the manual
prepared-job path; they are not an estimate of model repair cost or a complete
maintenance queue forecast. The preview does not reserve anything: another
writer can change the graph immediately after it returns. The actual mutation
must still use the current revision.

“Source” means a leaf actually stored and linked in this ledger. If the host
stored only extracted prose, the core cannot recover an unregistered user
utterance. Similar text, model confidence, source labels, or lexical matches
never create a dependency. These outputs carry
`trust_boundary:"retrieved_data"` and `semantics_verified:false`.

### `timeline`

All parameters are optional:

```json
{"query":"launch","as_of":1794160000,"limit":50,"cursor":"returned-next-cursor"}
```

- `query`: a literal, case-insensitive substring of permitted returned body
  text, or a record ID. At most 4096 UTF-8 bytes. No semantic search occurs.
  Suppressed bodies cannot match a query.
- `as_of`: a nonnegative Unix-second timestamp no later than the current time.
  Null or omission chooses the complete event history. A timestamp selects
  the last locally recorded event for each record at or before that second.
  This is the known-at-time view, not an occurrence-time filter.
- `limit`: 1–100, default 50.
- `cursor`: the previous `next_cursor`, passed unchanged with the same
  `query` and `as_of`. Limit may change. The cursor is opaque to clients.

The result contains `events`, `next_cursor` (null at the end), `has_more`,
`as_of`, `mode` (`event_history` or `knowledge_at_time`), `order`
(`newest_commit_first`), `commit_token`, `checked_at`, and `scanned`.

Each event includes:

```json
{
  "event_id":42,
  "type":"revision",
  "id":"record-id",
  "revision":2,
  "current_revision":3,
  "status_at_event":"active",
  "current_status":"active",
  "recorded_at":1794160000,
  "occurred_at":1794000000,
  "valid_from":1794000000,
  "valid_until":null,
  "commit_sequence":8,
  "history_id":"originating-ledger-history-uuid",
  "method":"revise",
  "time_quality":"local_commit",
  "kind":"assertion",
  "historical_only":true,
  "usable_as_current":false,
  "body_withheld_reason":null,
  "body":"Permitted historical assertion."
}
```

Event types are `revision`, `stale`, and `retracted`. Candidate submissions,
rejections, embedding jobs and other operations with no record revision or
lifecycle transition do not become timeline events. Use the unchanged
`changes` API for durable mutation invalidations. `source` and `fact_key` are
included only when a body is permitted.

Pagination pins the current global commit token. Any intervening durable
mutation, including one in another namespace, returns `stale_cursor`; restart
the query. Restore also invalidates old cursors. A cursor does not provide
authorization or an indefinitely held SQLite snapshot.

Pages scan at most 500 candidate events and return complete bodies within a
512 KiB response budget. An empty page may still have `has_more:true` after
filtering; follow its cursor. An impact preview fails rather than truncating
if it exceeds 100 revisions, 1000 registered ancestors/descendants, or 512 KiB.
SQLite descendant discovery still walks the registered closure before applying
the preview size bound; this is not an unbounded-graph performance guarantee.

## Three independent time meanings

All times use UTC Unix seconds.

1. **`occurred_at`** is the caller's attributed event occurrence time. It is
   optional on `remember` and `revise`. Omission on `remember` stays unknown
   (null). Omission on `revise` preserves the previous occurrence value;
   explicit null clears it. The core does not verify this claim or derive it
   from prose. It may represent a scheduled future event.
2. **`valid_from` / `valid_until`** describe the recorded applicability
   interval: inclusive start, exclusive end. Their existing immutable-record
   contract is unchanged.
3. **`recorded_at`** on a new event is the local mutation's recording time,
   finalized atomically with its commit. A late import is known locally only
   when imported, even when its source record has old timestamps.

Second-level cutoffs include all known events in that second. `event_id`
orders committed events and disambiguates same-second changes, but timestamps
are not subsecond clocks. A backwards host-clock adjustment can make wall time
non-monotonic; commit/event order remains authoritative. `as_of` is therefore
a recorded timestamp cutoff, not a proof of real-world event chronology.

## Historical bodies are not restored current facts

Current withdrawal is always applied before a historical body can be returned.
A withdrawn record and all its withdrawn descendants have no body through
these APIs, regardless of cutoff. The event table stores no body copies.

Current stale state, changed/withdrawn ancestors, unknown historical
dependencies, and time-window mismatch suppress bodies with an explicit
`body_withheld_reason`. New revision events capture the registered dependency
versions after the whole mutation finishes. Thus repairing a summary with new
sources does not make its old summary text visible again under the new graph.

A corrected source assertion may remain visible solely as historical evidence.
An assertion that was valid at the requested time but is expired now may also
be shown with `historical_only:true` and `usable_as_current:false`. The normal
`recall` interface is unchanged and never substitutes such historical bodies
for current context. A host must honor these flags and should keep audit
views separate from prompt-memory compilation.

The `knowledge_at_time` response includes lifecycle and ineligible metadata
to explain gaps; it does not claim that every returned event had an applicable
body at the cutoff. No API can retract text already delivered to a host,
screen capture, existing file, or model context.

## Migration, import and recovery

Opening schemas 0–4 migrates transactionally to schema 5 while retaining the
database history identity, existing sequence, receipts, fact identities,
bindings and vectors. Existing revision timestamps become
`time_quality:"legacy_revision"`; the current inactive state may add a
`legacy_current_state` event. Missing historical transitions, occurrence times,
commit identities and ordering are never invented.

Only the current legacy revision has recoverable current dependencies. Older
legacy revisions are metadata-only with `historical_dependencies_unknown`.
`legacy_history_complete:false` explicitly disclaims reconstruction of a
complete old event stream.

The existing `portable/1` bundle remains compatible and unchanged. It carries
current accepted content and candidate/withdrawal state, **not** these event
histories, old bodies, dependency snapshots or new occurrence metadata.
Destination events record local admission, and unavailable occurrence metadata
stays unknown. Import can therefore preserve knowledge without pretending to
preserve this new local audit history.

SQLite backup/restore preserves event metadata. Restore changes the current
history identity; historical events retain their originating `history_id` and
`commit_sequence`. Their identities are evidence, not usable current cursors.
As before, restore requires a current-schema backup; migrate a separate copy
of an older backup first. Schema 5 is not supported by older binaries.

## CLI integration

Existing `call METHOD --params '{"...":"..."}'` remains supported. For larger
requests:

```sh
recall-ledger --db example.db --namespace project call timeline \
  --params-file query.json
recall-ledger --db example.db --namespace project call bundle_plan \
  --params-file -
```

`--params-file -` reads stdin. Explicit `--params` and `--params-file` are
mutually exclusive; absent parameters default to `{}`. File/stdin JSON is
bounded to 1 MiB and parsed before the store operation. The client controls
stdin; the CLI neither guesses paths from JSON nor retries writes.

`call` also accepts `--expected-binding '{"namespace":"project","history_id":"…"}'`.
Runtime applies this binding inside every actual Store transaction, including
receipt replay. It is not replaced with a separate one-time preflight read.

## Local checks

`tests/history_and_impact.rs` covers source preservation, transitive impact,
read-only behavior, namespace/CAS denial, historical correction flags, repaired
summary withholding, withdrawal/search suppression, timestamp meaning,
pagination invalidation, schema-4 migration, backup/restore, CLI large-stdin
and atomic binding, and real temporary Unix-socket calls. These use synthetic
data, temporary databases and no model calls. See the task's final execution
record for actual command results.
