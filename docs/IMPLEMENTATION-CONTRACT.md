# v0.1 implementation contract

This is the implementation slice, not a claim that the research RFC is complete.
Rust core: `recall_ledger::Store::open(path: impl AsRef<Path>) -> Result<Store, Error>`;
`store.execute(namespace: &str, method: &str, params: serde_json::Value) -> Result<Value, Error>`.
Error is serializable `{code: String, message: String, retryable: bool}` and implements Display/Error.
Trusted transport configuration pins namespace; method parameters cannot override it.

All mutating requests require a nonempty `idempotency_key`. Same namespace/method/key
and different arguments conflict. Receipt and mutation commit atomically. Receipts
contain identifiers/status, never original body text. Keys persist in v0.1.

Methods:

- `status {}`: storage UUID, commit sequence, schema and counts/capabilities.
- `remember {body, kind?, source?, volatility?, valid_from?, valid_until?, idempotency_key}`.
  Kind defaults assertion; kinds assertion,inference,preference,episode,commitment,summary.
  Volatility stable,normal,volatile; default normal. Optional validity times are Unix seconds.
  Returns `{id,revision:1,commit_token,...}`.
- `inspect {id,history?:false}`: current record. Retracted returns not_found.
  History omits physically erased content; superseded revisions are historical only.
- `revise {id,expected_revision,body,idempotency_key}`: increments revision;
  stales all transitive derived descendants and invalidates affected prepared jobs atomically.
- `retract {id,expected_revision,idempotency_key}`: logical retraction and transitive
  withdrawal. No public body read afterward, including history/export/job/retrieval.
  SQLite page/WAL/backups secure erasure is NOT claimed.
- `recall {query?: "",limit?:10,max_chars?:4000,minimum_commit?:string}`:
  exact namespace, active current/temporally valid records only; literal-token FTS,
  deterministic fallback for CJK; no model or network. Returns `{items,budget,coverage,
  commit_token}`. Character budget is explicit, not a tokenizer claim.
- `prepare {inputs:[id,...],processor:string,idempotency_key}`: atomically captures
  active source revisions, creates a prepared job, returns `{job_id,dependencies,
  state,commit_token}`. No body text in receipt.
- `job {job_id}`: returns prepared state and input bodies only if all captured
  dependencies remain current/active/valid. Old job inputs never bypass retraction.
- `publish {job_id,body,kind?:"summary",idempotency_key}`: under one transaction,
  validates ALL source revisions including ancestry and validity, creates a derived
  record and immutable edges, marks job published. Late jobs fail stale_dependency.
  Processor must be trusted to use only supplied inputs; no claim to see hidden reads.
- `operation {method,idempotency_key}`: recover original receipt; unknown key -> not_found.
- `export {}`: schema + current readable records/edges; diagnostic snapshot, NOT backup/restore.
- `plan {strategy?:"topic",max_group_chars?:4000}`: deterministic consolidation
  groups within namespace/kind/source boundaries and bounded text; experimental, no task-gain claim.
  Strategies "individual" (one record per group), "fixed" (bounded chunks),
  "topic" (first whitespace token heuristic, not semantic clustering),
  "volatility" (explicit change-frequency labels). Hard scope remains fixed.

Library is cross-platform; Unix-domain IPC daemon is macOS/Linux in v0.1. CLI:
`recall-ledger --db PATH --namespace NAME call METHOD --params JSON`
or `serve --socket PATH`, `mcp --socket PATH` (MCP connects to running daemon).
IPC is newline JSON `{id,method,params}` -> `{id,result}` or `{id,error}`.
IPC/MCP frames are limited to 1 MiB including the newline. String request IDs
are limited to 256 UTF-8 bytes and rejected before execution when oversized.
Bodies are limited to 65,536 UTF-8 bytes; prepared jobs accept at most 64 inputs.
Large read responses can exceed the transport limit; use smaller recall budgets
or the trusted CLI/library for large diagnostic exports.
One daemon lock per DB, private socket permissions, bounded messages. No HTTP listener.
MCP stdio adapter pins backend; exposes remember/recall/inspect/revise/retract only.
CLI and Rust library are trusted administrative surfaces; namespace is not hostile same-UID isolation.
