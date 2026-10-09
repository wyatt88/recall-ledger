#!/usr/bin/env python3
"""Agent hook sketch; no model, framework, embeddings or automatic extraction.

Start the daemon first, then run:
PYTHONPATH=sdk/python python3 examples/python_agent.py --socket /tmp/recall.sock
"""

import argparse
import json
import uuid

from recall_ledger import Client


def before_turn(memory: Client, user_text: str) -> dict:
    """The host puts these records in an explicitly labelled untrusted-data block."""
    return memory.recall(user_text, max_chars=2000)


def after_turn(memory: Client, confirmed_decision: str, event_id: str) -> dict:
    """Persist only a decision the host has explicitly chosen to remember."""
    return memory.remember(
        confirmed_decision,
        kind="commitment",
        source="example:explicit-user-decision",
        volatility="stable",
        idempotency_key=event_id,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True)
    args = parser.parse_args()
    memory = Client(args.socket)
    # In a real host, persist this ID with the conversation event before the call.
    event_id = f"demo:{uuid.uuid4()}"
    receipt = after_turn(memory, "The implementation language is Rust.", event_id)
    restored = before_turn(memory, "Rust")
    print(json.dumps({"write_receipt": receipt, "before_turn": restored}, indent=2))


if __name__ == "__main__":
    main()
