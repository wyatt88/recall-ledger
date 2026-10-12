"""Dependency-free IPC client. The daemon, not this client, selects namespace."""

from .client import Client, MemoryError, TransportError

__all__ = [
    "Client", "MemoryError", "TransportError", "HandoffError",
    "prepare_handoff", "inspect_handoff", "verify_handoff",
    "save_handoff", "load_handoff", "rebase_handoff",
]


def __getattr__(name):
    # Keep ``python -m recall_ledger.handoff`` free of runpy's pre-import warning.
    if name in __all__[3:]:
        from . import handoff
        return getattr(handoff, name)
    raise AttributeError(name)
