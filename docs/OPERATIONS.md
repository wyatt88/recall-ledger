# Running local models and maintenance

RecallLedger is a source preview. The post-v0.4 development worker defaults to
[source-exact consolidation](fidelity.md), requiring no model server. For vector
retrieval or explicit model summaries, run your own OpenAI-compatible embedding
or chat model server. The project neither downloads models nor starts a cloud service.

## Configuration

Copy and edit [the example](../examples/models.local.json). Each configured
provider needs a complete endpoint URL, exact returned model name, and nonempty
weight `revision`. Set embedding `dimensions` to the output size, or omit it to
let the first stored vector bind that model key's dimension.

Only HTTP loopback endpoints are accepted: a literal loopback IP or exactly
`localhost`, which resolves here to `127.0.0.1`. For an IPv6-only server, write
`http://[::1]:PORT/...`. Remote/LAN hosts, HTTPS, URL credentials, query strings,
fragments, and redirects are rejected. Proxy environment variables are ignored.
There is no API-key field or alternate-provider fallback.

Embedding requests send a single `input`, `model`, `encoding_format="float"`,
and optional `dimensions`. A provider-specific `task_field` can pass the input
purpose. For an encoder using `input_type`, configure:

```json
{
  "task_field": "input_type"
}
```

The corresponding request carries `"input_type":"query"` or `"document"`.
This is an excerpt for the embedding object, not a complete configuration.

Summary requests are non-streaming chat completions with temperature 0, one
choice, and configured `max_tokens` (default 512). The system prompt treats source
records as untrusted data and asks for a bounded source-only summary. If your
server requires an option to disable reasoning output, its summary object can
include:

```json
{
  "extra_body": {
    "chat_template_kwargs": {
      "enable_thinking": false
    }
  }
}
```

`extra_body` is trusted local configuration, limited to 4 KiB. It cannot override
model/messages/temperature/token limits/streaming or add tools. Your server must
actually support the requested option. Empty, truncated, oversized, tool-call,
or wrong-model responses are rejected. No automatic truncation converts an
invalid generation into a successful summary.

Use immutable weight digests and encoding-configuration identifiers in
`revision`. This field is an operator declaration; a server can change weights
without reporting it. A changed embedding configuration gets another model key
and therefore requires indexing into that new space. Even timeout changes alter
the key because the complete provider configuration is hashed.

Summary recipes pin the complete summary configuration and prompt version.
An incompatible worker does not claim those tasks; status reports
`matching_model_configuration_required`. It does not silently regenerate old
recipes using a different model. Restore their original configuration, or
create a new explicit recipe under the new configuration.

## Start the daemon

```sh
mkdir -p .local-state
chmod 700 .local-state

./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  --models examples/models.local.json \
  serve --socket .local-state/memory.sock
```

The daemon runs a maintenance worker by default, including without models.
Each tick makes a bounded amount of progress and then waits before the next
tick. The interval is a pause between ticks, not an inference deadline.

| Control | Default | Meaning |
| --- | ---: | --- |
| `--worker-interval-ms` | 1000 | Pause between worker ticks |
| `--no-worker` | false | Disable all background maintenance |
| `--summary-policy` | `evidence` | Complete source preservation; `model` opts into automatic free-form summaries |
| `--auto-summary-min` | 4 | Minimum eligible records per automatic group; 0 disables discovery |
| `--summary-strategy` | `fixed` | `individual`, `fixed`, `topic`, or `volatility` |
| `--max-group-chars` | 12000 | Automatic group input-character budget, 1–16000 |
| `--max-summary-chars` | 16384 / 2000 | Evidence / model default; evidence allows 1–65536, model 1–16384 |

The default worker processes up to four configured embeddings and one recipe per tick.
Strict recipes have priority and never call a model. Existing model recipes
still require their exactly matching configured processor.
Automatic discovery is also bounded. Summary generation/repair precedes indexing
so a freshly published result can be embedded in the same tick. Model calls run
outside database transactions. Inference is sequential in this worker; it does
not imply parallel model execution.

Automatic groups preserve source/kind boundaries and contain currently eligible,
unmanaged root records. These are explicit rules, not learned salience or
importance judgments. The default threshold does not prevent explicit recipes
with fewer than four records. Source records remain stored after summarization.
Inputs previously assigned to cancelled recipes remain excluded from automatic
discovery, so retracting a managed summary does not silently recreate it on the
next tick. If its source records are still active, a deliberate `consolidate` or `summarize` call
with a new idempotency key can create a new recipe and a new output ID.

Clients cannot change model endpoints through request parameters. Start the MCP
adapter with the socket only; model configuration belongs to the daemon.

## Observe progress and enqueue a summary

The Python SDK defaults to a 330-second request timeout. Semantic recall can wait
on one local embedding call; summary enqueue returns before model generation.

```python
from recall_ledger import Client

memory = Client("/absolute/path/to/memory.sock")
first = memory.remember(
    "The project uses Rust.",
    source="example:project",
    idempotency_key="project-language",
)
second = memory.remember(
    "The memory database must run locally.",
    source="example:project",
    idempotency_key="project-storage",
)
queued = memory.summarize(
    [first["id"], second["id"]],
    max_chars=800,
    idempotency_key="project-summary",
)
print(queued)                       # Task and reserved output ID, not prose.
print(memory.maintenance_status())  # Wait for this task to become succeeded.
# Then: memory.inspect(queued["output_id"])
print(memory.recall("implementation constraints", mode="hybrid"))
```

The preceding example explicitly requests a model interpretation. For complete
source preservation, use `memory.consolidate(..., max_chars=16384)` instead and
audit the result with `memory.evidence_inspect(output_id)`. Default automatic
discovery follows that strict policy. Both paths keep sources; only the strict
path mechanically checks full source-text retention.

`status` reports configured model keys and indexing counts. `embedding_status`
reports indexing progress/failures for the configured key. Model status says
`health_checked=false`: a loaded configuration is not a server health check.

Always inspect retrieval `coverage`. Indexing is asynchronous. Hybrid query-model
failure can return lexical results with `degraded=true` and an error code;
vector-only queries return the model error. An incomplete index and a query
provider failure are different conditions.

## Manual maintenance

For a controlled single pass, stop the daemon worker or use a separate
administrative session with a deliberate processing budget:

```sh
./target/release/recall-ledger \
  --db .local-state/memory.sqlite3 --namespace demo \
  --models examples/models.local.json \
  maintain --max-embeddings 4 --max-summaries 1 --auto-summary-min 0
```

`maintain` returns actions, recorded errors, task state, and storage/model status.
A completed command can contain per-item errors; inspect its JSON result.
The maximum settings are 32 embeddings and 8 summaries per call. Its elapsed
time can be the sum of those bounded model requests.

Automatic discovery can be enabled on the command with `--auto-summary-min 4`;
`--summary-strategy`, `--max-group-chars`, and `--max-summary-chars` have the same
defaults as the daemon. The embedded runtime operation uses JSON field
`strategy` for the grouping strategy.

The socket API rejects `maintain`; the daemon owns its worker. A second trusted
CLI process can still access the database, so leases and revision checks remain
necessary. Overlapping passes can duplicate embedding inference even though
publication is fenced. There is no global cross-process compute budget.

## Failure and repair

Task states and last machine error codes are durable. Leases can be reclaimed
after expiry. A source revision revokes old work and queues another generation;
retraction cancels the affected recipes. In-flight model computation may finish
after cancellation, but its old lease cannot publish.

Retryable summary failures back off and stop after five recorded failures.
Non-retryable failures become `failed` immediately. After fixing the provider or
configuration, retry a failed active task explicitly:

```python
memory.call("maintenance_retry", {
    "task_id": "task-id-from-maintenance-status",
    "idempotency_key": "retry-task-after-provider-fix"
})
memory.call("embedding_retry", {
    "idempotency_key": "retry-index-after-provider-fix"
})
```

The embedding retry resets recorded failures for the configured model key.
Cancelled recipes are terminal. Manual `prepare`/`publish` outputs have no
automatic regeneration recipe. No failure path should make obsolete outputs
readable again.

## Data and release boundaries

Opening a schema 1/2 database upgrades it transactionally to schema 3. Before
upgrading important data, take a consistent SQLite backup with writers stopped
or SQLite's backup mechanism. Do not copy only the main file while WAL writes
are active. Version 0.3 supplies [consistent backup and new-history restoration](recovery.md).
Old binaries cannot open the upgraded database. A v0.3 backup must use schema 3;
for an older snapshot, migrate a separate copy first. Restore requires a fresh
target path with no preexisting SQLite sidecars.

The [Claude Code adapter](claude-code.md) supplies project hooks, explicit
Markdown migration and recoverable settings changes. Its default uninstall
keeps native auto memory disabled so old notes are not silently reactivated.
Use [candidate admission](ingestion.md) to review and resolve captured proposals.
Freshness feeds and manifests are documented in the [contract](CONTRACT.md).

Local [ChatGPT](chatgpt.md) and [Codex](codex.md) share one project hook adapter.
[OpenCode V1](opencode.md) uses a project plugin; [KiroCrew](kirocrew.md) uses
a bridge called by trusted Gateway code. Each persists the daemon's namespace
and history identity and sends it with every operation. A restored database
has a different history; recover or review the preserved state instead of
regenerating bindings and automatically replaying old queues. MCP deployments
should keep a saved `--binding-file` for the same protection across restarts.

Deletion clears managed records and indexes, not physical SQLite pages, model
server logs, backups, or copies already delivered to agents. Locality alone is
not a retention policy for a separately operated model server.

Generated summaries still need application-level quality evaluation. Neither
HTTP contract tests nor successful generation prove factual accuracy, robust
instruction isolation, a benchmark gain, or production readiness.
