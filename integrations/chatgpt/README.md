# Local ChatGPT Agent App

`recall-ledger-chatgpt` is an alias of the local Codex harness adapter.
It uses the same selected project's `.codex/hooks.json`, `.codex/config.toml`
and `.codex/recall-ledger/` state. Running both aliases does not create two
hook sets.

This entry point applies only when the local Agent App exposes the documented
Codex project hooks and native-memory configuration. Its name is not evidence
that every ChatGPT App surface supports them. Hook trust, effective settings
and live-session acceptance must be checked in the actual local App.

See [local ChatGPT integration](../../docs/chatgpt.md) and
[the shared Codex protocol](../../docs/codex.md). There is no cloud Actions
transport or public memory endpoint in this adapter.
