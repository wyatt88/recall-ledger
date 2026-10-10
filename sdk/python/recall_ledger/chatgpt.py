"""Local ChatGPT Agent entry point sharing the Codex harness installation.

This alias creates no second hook set and has no cloud/Actions transport.
Only use it where the local app exposes the documented Codex project hooks.
"""

from .codex import main


if __name__ == "__main__":
    raise SystemExit(main())
