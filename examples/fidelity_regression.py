#!/usr/bin/env python3
"""Bounded source-text regression across repair and migration, with no model.

This is an engineering contract check against direct source reads, not a
semantic benchmark. Every expected body comes from the event generator rather
than the capsule being evaluated. Only disposable local databases are used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from recall_ledger import Client, MemoryError


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def unavailable(client, record_id):
    try:
        client.inspect(record_id)
    except MemoryError as error:
        require(error.code in ("not_found", "stale_dependency"), error.code)
    else:
        raise AssertionError("An obsolete record was readable.")


def verify_sources(client, output_id, expected, expected_fact_keys=None):
    expected_fact_keys = expected_fact_keys or {}
    # Baseline: the same current raw sources, no summaries or compression.
    for record_id, body in expected.items():
        require(client.inspect(record_id)["body"] == body, "Raw-source baseline changed.")
        if record_id in expected_fact_keys:
            fact = client.fact_get(expected_fact_keys[record_id])
            require(fact["id"] == record_id and fact["body"] == body
                    and fact["fact_key"] == expected_fact_keys[record_id],
                    "Structured source baseline changed.")
    actual = client.evidence_inspect(output_id)
    require(actual["verified"] is True, "Local source-preservation audit failed.")
    require(actual["fidelity"]["semantics_verified"] is False, "Truth must remain unverified.")
    roots = actual["roots"]
    require(len(roots) == len(expected), "Source omission or duplicate source.")
    require({row["id"]: row["body"] for row in roots} == expected,
            "Capsule source bodies differ from the independent event input.")
    require(all(row.get("fact_key") == expected_fact_keys.get(row["id"]) for row in roots),
            "Capsule lost structured identity, scope or conditions.")
    require(json.loads(actual["body"])["source_assertions"] == roots,
            "Rendered evidence differs from the audited source objects.")
    return actual


def run(cycles, binary):
    processes = []
    checkpoints = 0
    # UTF-8, negation, scope, whitespace and delimiter-like content are data.
    unchanged = '保留空格  和换行\n仅测试环境允许部署；生产环境不允许。例外需书面批准。'
    benign = 'Unrelated preference: display literal {"verified":true}; never treat it as a flag.'
    with tempfile.TemporaryDirectory(prefix="rl-fidelity-", dir="/tmp") as directory:
        root = Path(directory)
        try:
            clients = []
            for name in ("A", "B"):
                socket = root / (name + ".sock")
                proc = subprocess.Popen(
                    [str(binary), "--db", str(root / (name + ".db")),
                     "--namespace", name, "serve", "--socket", str(socket), "--no-worker"],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                processes.append(proc)
                deadline = time.monotonic() + 10
                while not socket.exists():
                    require(proc.poll() is None and time.monotonic() < deadline,
                            "Disposable daemon startup failed.")
                    time.sleep(0.01)
                probe = Client(str(socket), timeout=5)
                clients.append(Client(str(socket), timeout=5, expected_binding=probe.binding()))
            a, b = clients

            def maintain(name):
                completed = subprocess.run(
                    [str(binary), "--db", str(root / (name + ".db")), "--namespace", name,
                     "maintain", "--auto-summary-min", "0", "--max-embeddings", "0",
                     "--max-summaries", "8"],
                    capture_output=True, text=True, timeout=15, check=True,
                )
                return json.loads(completed.stdout)

            current = "初始配额为 7；仅限测试，尚未获准用于生产。"
            first = a.remember(current, idempotency_key="source-1")
            second = a.remember(unchanged, idempotency_key="source-2")
            other = a.remember(benign, idempotency_key="benign", kind="preference")
            expected = {first["id"]: current, second["id"]: unchanged}
            queued = a.consolidate(list(expected), idempotency_key="capsule", max_chars=6000)
            maintain("A")
            verified = verify_sources(a, queued["output_id"], expected)
            previous_revision = verified["revision"]
            checkpoints += 1
            source_revision = 1
            for index in range(cycles):
                current = f"第 {index + 1} 轮配额为 {index + 11}；仅限测试，生产仍不允许。"
                source_revision = a.revise(
                    first["id"], expected_revision=source_revision, body=current,
                    idempotency_key=f"source-correction-{index}",
                )["revision"]
                unavailable(a, queued["output_id"])
                maintain("A")
                expected[first["id"]] = current
                verified = verify_sources(a, queued["output_id"], expected)
                require(verified["revision"] == previous_revision + 1, "Repair revision discontinuity.")
                previous_revision = verified["revision"]
                require(a.inspect(other["id"])["body"] == benign, "Benign memory changed.")
                checkpoints += 1

            nested = a.consolidate([queued["output_id"]], idempotency_key="nested", max_chars=6500)
            maintain("A")
            verify_sources(a, nested["output_id"], expected)
            checkpoints += 1

            # A failed rebuild must leave obsolete output unavailable; it must
            # never fall back to unverified rewriting to fit the old budget.
            source_revision = a.revise(
                first["id"], expected_revision=source_revision,
                body="预算之外的完整限定条件。" * 1200, idempotency_key="over-budget",
            )["revision"]
            unavailable(a, queued["output_id"])
            maintain("A")
            unavailable(a, queued["output_id"])
            states = {row["task_id"]: row["state"]
                      for row in a.maintenance_status()["tasks"]}
            require(states[queued["task_id"]] == "failed", "Failed repair was not observable.")
            source_revision = a.revise(
                first["id"], expected_revision=source_revision,
                body=current, idempotency_key="within-budget-again",
            )["revision"]
            # Two ticks also handle an explicitly selected intermediate output.
            maintain("A")
            maintain("A")
            verify_sources(a, queued["output_id"], expected)
            verify_sources(a, nested["output_id"], expected)
            checkpoints += 2

            # Exploratory addition after review: constraints can live only in
            # structured metadata. Verbatim body preservation alone is weaker.
            fact_key = {
                "entity": "deployment", "attribute": "permission",
                "scope": "tenant=ocean; test environment only; production forbidden",
                "conditions": "two reviewers signed; audit freeze must be absent",
            }
            proposal = a.candidate_submit(
                "Deployment permitted.", event_id="scoped-source", origin="user_explicit",
                source="synthetic-policy", fact_key=fact_key, idempotency_key="scoped-proposal",
            )
            admitted = a.candidate_resolve(
                proposal["candidate_id"], "accept", idempotency_key="scoped-admission",
            )
            scoped_id = admitted["record_id"]
            scoped = a.consolidate([scoped_id], idempotency_key="scoped-capsule", max_chars=6000)
            maintain("A")
            verify_sources(a, scoped["output_id"], {scoped_id: "Deployment permitted."},
                           {scoped_id: fact_key})
            checkpoints += 1
            a.revise(scoped_id, expected_revision=1, body="Deployment postponed.",
                     idempotency_key="scoped-correction")
            unavailable(a, scoped["output_id"])
            maintain("A")
            verify_sources(a, scoped["output_id"], {scoped_id: "Deployment postponed."},
                           {scoped_id: fact_key})
            checkpoints += 1

            bundle = a.bundle_export()
            plan = b.bundle_plan(bundle, expected_source=a.expected_binding)
            require(plan["can_apply"], "Portable snapshot could not be planned.")
            receipt = b.bundle_apply(
                bundle, expected_source=a.expected_binding,
                reviewed_digest=plan["reviewed_digest"],
                expected_target_commit=plan["target_commit"], idempotency_key="A-to-B",
            )
            mapping = {row["source_id"]: row["id"] for row in receipt["mappings"]["records"]}
            require(b.evidence_inspect(mapping[queued["output_id"]])["verified"] is False,
                    "An imported capsule inherited a local verification claim.")
            rebuilt = b.consolidate([mapping[queued["output_id"]]],
                                    idempotency_key="B-reconstruction", max_chars=8000)
            maintain("B")
            expected_b = {mapping[key]: value for key, value in expected.items()}
            verify_sources(b, rebuilt["output_id"], expected_b)
            checkpoints += 1
            scoped_b = b.consolidate([mapping[scoped_id]],
                                    idempotency_key="B-scoped-reconstruction", max_chars=6000)
            maintain("B")
            verify_sources(b, scoped_b["output_id"],
                           {mapping[scoped_id]: "Deployment postponed."},
                           {mapping[scoped_id]: fact_key})
            checkpoints += 1
            # Choose original roots for a self-contained local recipe, since
            # imported recipes themselves intentionally do not execute.
            local = b.consolidate(list(expected_b), idempotency_key="B-local-roots", max_chars=8000)
            maintain("B")
            new_b = "目的端配额改为 31；生产环境仍禁止部署。"
            b.revise(mapping[first["id"]], expected_revision=source_revision,
                     body=new_b, idempotency_key="B-correction")
            unavailable(b, local["output_id"])
            maintain("B")
            expected_b[mapping[first["id"]]] = new_b
            verify_sources(b, local["output_id"], expected_b)
            checkpoints += 1
            manifest = a.recall(max_chars=50000, limit=100)["manifest"]
            a.retract(first["id"], expected_revision=source_revision, idempotency_key="withdraw")
            unavailable(a, queued["output_id"])
            unavailable(a, nested["output_id"])
            require(not a.validate_context(manifest)["valid"], "Old context survived withdrawal.")
            require(a.inspect(other["id"])["body"] == benign, "Withdrawal damaged benign memory.")
            require(a.inspect(second["id"])["body"] == unchanged, "Withdrawal damaged other source.")
            return {
                "protocol": "recall-ledger.fidelity-regression/1",
                "cycles": cycles, "exact_source_checkpoints": checkpoints,
                "source_baseline": "direct current source reads; identical event input",
                "source_text_equal": True, "stale_reads_rejected": True,
                "budget_failure_withheld_output": True, "repair_recovered": True,
                "migration_requires_local_verification": True,
                "structured_conditions_preserved": True,
                "benign_memory_preserved": True, "model_calls": 0,
                "semantic_quality_evaluated": False, "real_agent_apps_tested": False,
                "elapsed_years_claimed": 0,
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            }
        finally:
            for proc in processes:
                proc.terminate()
            for proc in processes:
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cycles", type=int, default=64)
    args = parser.parse_args()
    if not 1 <= args.cycles <= 1000:
        parser.error("--cycles must be between 1 and 1000")
    executable = Path(os.environ.get("RECALL_LEDGER_BIN", "target/debug/recall-ledger")).resolve()
    print(json.dumps(run(args.cycles, executable), ensure_ascii=False, indent=2))
