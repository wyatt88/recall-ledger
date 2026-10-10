# Local Codex adapter

Use `recall-ledger-codex install --project /absolute/project --socket
/absolute/private/memory.sock` after installing the Python SDK with Python
3.11+. The installer merges project hooks and creates a recovery journal;
it never grants host trust.

See [the complete installation, privacy and rollback contract](../../docs/codex.md).
The JSON files in this directory are **synthetic stdin fixtures**, not real
user transcripts or proof that an installed Codex version emitted them.

The lifecycle tests run those envelope shapes against a real local daemon.
Hook trust, effective configuration and live-session acceptance remain separate.
