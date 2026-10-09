"""Dependency-free IPC client. The daemon, not this client, selects namespace."""

from .client import Client, MemoryError, TransportError

__all__ = ["Client", "MemoryError", "TransportError"]
