# KiroCrew Gateway integration

[`gateway_bridge.py`](gateway_bridge.py) exposes callable helpers for trusted
Gateway code before model dispatch and after an admitted assistant event.
Use a frozen `FrozenContext`, a trusted state-directory lookup and stable
source-span IDs. Do not derive the member, store or mode from model output or
ScriptHookStore's untrusted stdin.

`recall-ledger-kirocrew install` prepares a binding and an MCP configuration
example; `recall-ledger-kirocrew call` is an executable Gateway subprocess
interface. Neither command registers an App, patches upstream KiroCrew or
replaces its native Memory V2.

See [the protocol, commands and remaining upstream work](../../docs/kirocrew.md).
