#!/usr/bin/env python3
"""Run a synthetic A -> B -> A migration and handoff with two real local daemons.

PYTHONPATH=sdk/python RECALL_LEDGER_BIN=target/debug/recall-ledger \
  python3 examples/migration_handoff.py

Only disposable databases are touched. No model or external service is used.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from recall_ledger import Client
from recall_ledger.handoff import deserialize_handoff, serialize_handoff


def transfer(source, target, key):
    bundle = source.bundle_export()
    expected = source.expected_binding
    preview = target.bundle_plan(bundle, expected_source=expected)
    assert preview["can_apply"], preview["conflicts"]
    receipt = target.bundle_apply(
        bundle, expected_source=expected,
        reviewed_digest=preview["reviewed_digest"],
        expected_target_commit=preview["target_commit"], idempotency_key=key,
    )
    return bundle, receipt


def run():
    binary = Path(os.environ.get("RECALL_LEDGER_BIN", "target/debug/recall-ledger")).resolve()
    processes = []
    # Short socket paths also work under macOS's Unix-domain path limit.
    with tempfile.TemporaryDirectory(prefix="recall-handoff-", dir="/tmp") as directory:
        root = Path(directory)
        try:
            clients = []
            for name in ("A", "B"):
                socket = root / (name + ".sock")
                process = subprocess.Popen(
                    [str(binary), "--db", str(root / (name + ".db")), "--namespace", name,
                     "serve", "--socket", str(socket), "--no-worker"],
                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                )
                processes.append(process)
                deadline = time.monotonic() + 10
                while not socket.exists():
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("Disposable memory daemon did not start.")
                    time.sleep(0.02)
                unbound = Client(str(socket))
                clients.append(Client(str(socket), expected_binding=unbound.binding()))
            a, b = clients
            original = a.remember(
                "The project uses Rust. Deployment is allowed only after review.",
                idempotency_key="synthetic-source",
            )
            a.candidate_submit(
                event_id="unreviewed-proposal", body="Consider a cloud deployment later.",
                origin="model_inference", source="synthetic-demo",
                idempotency_key="synthetic-pending",
            )
            task = {
                "task_id": "local-memory-portability",
                "goal": "Continue development using the reviewed language decision.",
                "progress": ["The language decision is recorded."],
                "open_questions": ["Which deployment environment will be approved?"],
                "artifacts": [{"uri": "workspace:README.md", "description": "Review locally."}],
                "environment_checks": [{"condition": "Confirm the repository and branch."}],
            }
            packet_a = a.prepare_handoff(task, a.recall()["manifest"])
            # JSON round trip simulates transfer of a file to another local host.
            packet_a = deserialize_handoff(serialize_handoff(packet_a))
            bundle_a, receipt_b = transfer(a, b, "A-to-B")
            imported_id = next(
                row["id"] for row in receipt_b["mappings"]["records"]
                if row["source_id"] == original["id"]
            )
            packet_b = b.rebase_handoff(packet_a, bundle_a)
            assert b.verify_handoff(packet_b)["evidence_valid"]
            assert b.candidate_list(state="pending")["candidates"][0]["state"] == "pending"
            b.revise(
                imported_id, expected_revision=1,
                body="The project uses Rust. The approved deployment target is local only.",
                idempotency_key="B-correction",
            )
            assert not b.verify_handoff(packet_b)["evidence_valid"]
            task["progress"].append("The deployment decision is now local only.")
            new_b = b.prepare_handoff(task, b.recall()["manifest"])
            bundle_b, receipt_a = transfer(b, a, "B-to-A")
            returned = a.rebase_handoff(new_b, bundle_b)
            assert a.verify_handoff(returned)["evidence_valid"]
            assert a.inspect(original["id"])["revision"] == 2
            b.retract(imported_id, expected_revision=2, idempotency_key="B-withdrawal")
            transfer(b, a, "B-withdrawal-to-A")
            assert not a.verify_handoff(returned)["evidence_valid"]
            print(json.dumps({
                "scenario": "synthetic A -> B -> A across two real daemons",
                "round_trip_identity_preserved": True,
                "pending_candidate_preserved": True,
                "revision_after_return": 2,
                "stale_handoff_rejected": True,
                "withdrawal_propagated": True,
                "execution_authorized": False,
                "real_agent_apps_tested": False,
                "return_commit": receipt_a["commit_token"],
            }, indent=2))
        finally:
            for process in processes:
                process.terminate()
            for process in processes:
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                if process.stderr:
                    process.stderr.close()


if __name__ == "__main__":
    run()
