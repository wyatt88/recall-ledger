# Security policy

## Supported scope

RecallLedger 0.1 is an early local-storage implementation. Security fixes target
the latest source on the main branch. There is no long-term support commitment
for historical snapshots.

The local daemon is intended to run beside a trusted agent under the same
operating-system user. It uses a Unix-domain socket and has no HTTP listener.
Protect the database directory and socket directory with operating-system
permissions.

## Trust boundaries

- Namespace binding prevents a request from choosing another configured
  namespace. It does not isolate hostile processes running as the same user.
- Direct Rust library calls and CLI database access are trusted administrative
  access.
- Processors must account for their actual inputs. The store cannot observe
  hidden prompts, unreported tool reads, or model training data.
- Stored text can contain malicious instructions. A host must treat recalled
  content as data, not higher-priority instructions or tool authorization.
- Retraction blocks managed reads and clears managed body content. It does not
  securely erase SQLite pages, WAL files, backups, operating-system caches, or
  copies already delivered to clients.
- Database encryption, remote authentication, multi-user hosting, and hostile
  plugin isolation are not provided.
- The core does not call a model or send memory to a network service. A host or
  externally supplied processor may do so under its own configuration.

Do not use diagnostic exports as a secure backup or restore mechanism. Protect
exports and logs according to the sensitivity of their content.

## Reporting a vulnerability

Do not include private memory contents, credentials, or an exploit against a live
third-party deployment in a public issue.

If the repository's Security tab offers **Report a vulnerability**, use that
private channel. Otherwise, open a minimal public issue asking for a private
reporting channel, without publishing the exploit details or sensitive data.
This document does not claim that GitHub private reporting is already enabled.

A useful report includes the affected commit, platform, minimal synthetic
reproduction, expected boundary, and observed behavior. There is no guaranteed
response-time service level.
