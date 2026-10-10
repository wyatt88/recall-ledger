# v0.4 integration review

Review date: 2026-10-10. Separate AI engineering/review roles inspected the
implementation; role separation is not independent statistical evidence.
The root integrator examined the changes and ran the final complete checks
recorded in [validation](VALIDATION-v0.4.md).

## Resolved findings

| Finding | Correction and check |
| --- | --- |
| A reused socket could send old queues to a different namespace/history | Added persisted binding assertions, checked inside each store transaction before reads, receipts and writes; actual-daemon and MCP replacement tests |
| A preliminary binding read alone has a time-of-check/time-of-use gap | Assertion travels on every request and through runtime model work; injected real SDK clients must have the installed assertion |
| Concurrent first installs could replace the winning destination or Crew identity | Project/state locks re-read the winning configuration and compare database and Gateway identity |
| Installer could overwrite a user edit made after patch generation | Pass original bytes as `expected_before`; verify all targets under the adapter lock before writing config, journal or patches; 3 cases independently rerun |
| OpenCode could drop a capture when its initial diagnostic met an outage | Preserve it under the original binding; actual installed JS offline/recovery checks |
| Replacing OpenCode's system array did not update the reference held by the host | Mutate the output array in place; fixture and real bridge checks |
| Source checkout worked but an ordinary wheel lacked JavaScript | Package one canonical runtime and test wheel installation/execution outside checkout |
| Subprocess output decoding could split multibyte text | Buffer bytes before UTF-8 decoding |

The final narrow reviews found no remaining release-blocking issue in the
paths inspected. Six stdio MCP cases ran separate real processes. The final
configuration-race review independently ran the three new cases with temporary
files and fake clients; it did not claim they were host or daemon tests.

## Scope decisions

ChatGPT denotes the local Agent App using the supported local Codex runtime.
Its CLI alias shares a project installation with Codex and does not create a
second capture loop. Lifecycle hooks and optional stdio MCP serve different
purposes: hooks perform fresh automatic reads/capture; MCP exposes explicit
operations. MCP alone does not replace native memory.

KiroCrew's reviewed development Memory V2 is already a strong native baseline:
member/store identity, versioning, provenance, consolidation and privacy. The
bridge is callable SDK/CLI infrastructure plus a Gateway helper. It has not
been wired into an upstream deployment and does not claim one-click V2
replacement. OpenCode support is V1 v1.18.35, with source-specific hooks; V2
needs a separate implementation.

## Remaining boundaries

- Saved binding is a destination-consistency assertion, not same-OS-user
  authentication. MCP without a binding file selects identity again on restart.
- Host trust, effective hooks/settings, actual private mode, real compaction,
  subagent coverage and warm-session behavior need acceptance in the installed
  App. The generated settings and fixture passes do not establish those facts.
- Codex/ChatGPT private behavior uses an explicit adapter policy; it cannot infer
  a native private-chat toggle from the documented hook envelope. Independently
  exposed MCP tools need their own trusted privacy routing.
- Retrieved bodies enter host developer/system context with an advisory data
  label, not formal low-trust-role isolation. Already delivered prompts,
  transcripts and external copies cannot be retracted by the ledger.
- Queued proposals contain text and are not encrypted. Privacy is checked before
  new capture, but older state, backups and OS storage need their own retention
  policy. Configuration journaling is recoverable, not one filesystem-wide
  transaction.
- No accuracy/latency/cost advantage, paper reproduction or E4 host deployment
  is established by these engineering tests.
