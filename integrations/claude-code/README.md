# Claude Code project adapter

The implementation ships with the standard-library Python SDK as
`recall-ledger-claude`. It connects command hooks to an already running local
Rust daemon. It does not install a daemon service or call a model.

From the RecallLedger checkout:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install ./sdk/python
```

Choose an isolated project and the absolute socket path of its daemon:

```sh
.venv/bin/recall-ledger-claude install \
  --project /absolute/path/to/isolated-project \
  --socket /absolute/path/to/memory.sock
```

This explicitly edits that project's `.claude/settings.local.json`: it preserves
existing settings and hooks, disables native auto memory, and adds
`UserPromptSubmit` and `Stop` command hooks. It creates private adapter state in
`.claude/recall-ledger/`. Review and exclude both local paths from version control.
The Python environment must remain at the installed location.

See [the full guide](../../docs/claude-code.md) before installation for migration,
candidate review, failures, context validation and safe uninstall.

Engineering checks:

```sh
cargo build
PYTHONPATH=sdk/python RECALL_LEDGER_BIN="$PWD/target/debug/recall-ledger" \
  python3 -m unittest discover -s sdk/python/tests -v
```

These checks create temporary fixture projects and real local daemon processes.
They do not launch a Claude model session or inspect your existing memory.
