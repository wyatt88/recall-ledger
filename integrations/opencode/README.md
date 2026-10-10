# OpenCode V1 adapter

This is executable integration code for OpenCode **v1.18.35**, not just an MCP
configuration. It injects fresh, binding-pinned memory before agent model requests
and captures visible chat text as **pending proposals**. OpenCode V2 uses an
incompatible plugin API and is not supported by this entrypoint.

The Python wheel bundles the complete JavaScript runtime. With the distribution
installed, use `recall-ledger-opencode install --project ... --socket ...`.
The `runtime.mjs` in this directory is a development re-export; the canonical
runtime is `sdk/python/recall_ledger/resources/opencode_runtime.mjs`.

From the RecallLedger checkout, after starting a daemon:

```sh
PYTHONPATH=sdk/python python3 -m recall_ledger.opencode install \
  --project /absolute/path/to/project \
  --socket /absolute/path/to/memory.sock
```

Restart OpenCode. Installation reads and pins the daemon's namespace and history
identity. It writes only a new `.opencode/plugins/recall-ledger.js` entry and the
adapter's `.opencode/recall-ledger/` directory. It leaves existing configuration,
AGENTS.md and other plugins alone. Install a separate adapter per project.

To remove the entry and disable an already-loaded instance:

```sh
PYTHONPATH=sdk/python python3 -m recall_ledger.opencode uninstall \
  --project /absolute/path/to/project
```

The uninstall receipt and pending delivery state are retained. Reinstalling the
same binding recovers the entry; edited adapter files are never overwritten
silently. Restart the host after installing or uninstalling.

See [the full contract](../../docs/opencode.md) for capture limits, transient
context and compaction boundaries, recovery, and test commands. No live model
session or OpenCode desktop acceptance is implied by the protocol fixtures.
