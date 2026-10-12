import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from recall_ledger import Client, HandoffError, MemoryError
from recall_ledger import handoff
from test_host_runtime import DaemonFixture

BINDING = {"namespace": "fixture", "history_id": "12345678-1234-5678-9234-567812345678"}
MANIFEST = {
    "schema_version": "recall-ledger.context/1", "namespace": "fixture",
    "commit_token": BINDING["history_id"] + ":4",
    "records": [{"id": "source-1", "revision": 2}],
}
TASK = {
    "task_id": "task-language-migration",
    "goal": "完成 Rust 迁移 🧠",
    "progress": ["接口已经检查"],
    "open_questions": ["是否需要保留兼容层？"],
    "artifacts": [{"uri": "file:///unopened/design.md", "description": "Review before relying on it", "sha256": "a" * 64}],
    "environment_checks": [{"condition": "The target daemon belongs to the intended project", "reason": "Paths alone do not establish identity"}],
}


class HandoffTests(unittest.TestCase):
    def setUp(self):
        self.client = Client("/unopened/socket", expected_binding=BINDING)
        self.validation = {
            "valid": True, "invalid": [], "checked_at": 100,
            "commit_token": BINDING["history_id"] + ":5",
        }
        self.mock = patch.object(self.client, "validate_context", return_value=self.validation).start()

    def tearDown(self):
        patch.stopall()

    def prepare(self):
        return self.client.prepare_handoff(TASK, MANIFEST)

    def resign(self, packet):
        packet["digest"] = hashlib.sha256(handoff._canonical(handoff._payload(packet))).hexdigest()
        return packet

    def test_unicode_roundtrip_and_inspection_does_not_verify_or_authorize(self):
        packet = self.prepare()
        raw = handoff.serialize_handoff(packet)
        self.assertIn("完成 Rust".encode(), raw)
        decoded = handoff.deserialize_handoff(raw)
        self.assertEqual(packet, decoded)
        inspection = handoff.inspect_handoff(decoded)
        self.assertTrue(inspection["integrity_valid"])
        self.assertFalse(inspection["evidence_verified"])
        self.assertFalse(inspection["sender_authenticated"])
        self.assertFalse(inspection["execution_authorized"])
        self.assertEqual(self.mock.call_count, 1)
        self.assertEqual(self.mock.call_args.args, (MANIFEST,))
        TASK_COPY = copy.deepcopy(TASK)
        inspection["task"]["goal"] = "Changed"
        self.assertEqual(packet["task"], TASK_COPY)

    def test_verification_is_fresh_and_reports_environment_artifacts_pending(self):
        packet = self.prepare()
        result = self.client.verify_handoff(packet)
        self.assertTrue(result["evidence_valid"])
        self.assertTrue(result["requires_environment_review"])
        self.assertTrue(result["requires_artifact_review"])
        self.assertFalse(result["environment_verified"])
        self.assertFalse(result["artifacts_verified"])
        self.assertFalse(result["task_semantics_verified"])
        self.assertFalse(result["execution_authorized"])
        self.assertFalse(result["task_replayed"])
        self.assertEqual(self.mock.call_count, 2)
        self.validation.update(valid=False, invalid=[{"id": "source-1", "reason": "revision_changed"}])
        self.assertFalse(self.client.verify_handoff(packet)["evidence_valid"])
        with self.assertRaisesRegex(HandoffError, "stale_handoff_evidence"):
            self.prepare()

    def test_unbound_wrong_namespace_and_history_are_rejected_before_rpc(self):
        packet = self.prepare()
        self.mock.reset_mock()
        for client in (
            Client("/unopened/socket"),
            Client("/unopened/socket", expected_binding=dict(BINDING, namespace="other")),
            Client("/unopened/socket", expected_binding=dict(BINDING, history_id="87654321-4321-5678-9234-567812345678")),
        ):
            with self.subTest(binding=client.expected_binding):
                with self.assertRaises(HandoffError):
                    client.verify_handoff(packet)
        self.mock.assert_not_called()

    def test_digest_tampering_unknown_fields_and_policy_escalation_are_rejected(self):
        original = self.prepare()
        changed = copy.deepcopy(original)
        changed["task"]["goal"] = "Tampered"
        with self.assertRaisesRegex(HandoffError, "handoff_digest_mismatch"):
            handoff.inspect_handoff(changed)
        for patcher in (
            lambda value: value["task"].update(command="do not execute"),
            lambda value: value["policy"].update(tool_authorization=True),
            lambda value: value["policy"].update(tool_authorization=0),
            lambda value: value["evidence"]["manifest"]["records"].append({"id": "source-1", "revision": 2}),
        ):
            changed = copy.deepcopy(original)
            patcher(changed)
            self.resign(changed)
            with self.assertRaises(HandoffError):
                handoff.inspect_handoff(changed)
        # A checksum can be recomputed by anyone: never equate it with identity.
        forged = copy.deepcopy(original)
        forged["task"]["goal"] = "Different untrusted narrative"
        self.assertFalse(handoff.inspect_handoff(self.resign(forged))["sender_authenticated"])

    def test_size_duplicates_and_nonfinite_json_are_rejected_without_rpc(self):
        self.mock.reset_mock()
        huge = dict(TASK, progress=["a" * 4000] * 20)
        with self.assertRaisesRegex(HandoffError, "handoff_too_large"):
            self.client.prepare_handoff(huge, MANIFEST)
        for data in (
            b" " * (handoff.MAX_HANDOFF_BYTES + 1),
            b'{"schema_version":"a","schema_version":"b"}',
            b'{"value":NaN}',
            b"\xff",
        ):
            with self.subTest(data=data[:40]):
                with self.assertRaises(HandoffError):
                    handoff.deserialize_handoff(data)
        self.mock.assert_not_called()

    def test_bad_manifests_are_rejected_before_rpc(self):
        for transform in (
            lambda value: value.update(namespace="other"),
            lambda value: value.update(commit_token="other-history:4"),
            lambda value: value["records"][0].update(revision=True),
            lambda value: value["records"][0].update(revision=0),
            lambda value: value.update(records=[{"id": str(i), "revision": 1} for i in range(101)]),
        ):
            manifest = copy.deepcopy(MANIFEST)
            transform(manifest)
            with self.assertRaises(HandoffError):
                self.client.prepare_handoff(TASK, manifest)
        self.mock.assert_not_called()

    def test_file_save_is_private_exclusive_and_symlinks_are_refused(self):
        packet = self.prepare()
        with tempfile.TemporaryDirectory(prefix="rl-handoff-") as temp:
            root = Path(temp).resolve()
            target = root / "交接.json"
            saved = handoff.save_handoff(packet, target)
            self.assertEqual(saved["digest"], packet["digest"])
            self.assertEqual(target.stat().st_mode & 0o777, 0o600)
            self.assertEqual(handoff.load_handoff(target), packet)
            original = target.read_bytes()
            with self.assertRaisesRegex(HandoffError, "handoff_output_exists"):
                handoff.save_handoff(packet, target)
            self.assertEqual(target.read_bytes(), original)
            linked = root / "linked.json"
            linked.symlink_to(target)
            with self.assertRaisesRegex(HandoffError, "symlink_path_refused"):
                handoff.load_handoff(linked)

    def test_explicit_rebase_uses_verified_mapping_and_preserves_original_packet(self):
        packet = self.prepare()
        original = copy.deepcopy(packet)
        target_binding = dict(BINDING, namespace="target", history_id="87654321-4321-5678-9234-567812345678")
        target = Client("/unopened/target", expected_binding=target_binding)
        target_manifest = {
            "schema_version": "recall-ledger.context/1", "namespace": "target",
            "commit_token": target_binding["history_id"] + ":8",
            "records": [{"id": "mapped-record", "revision": 3}],
        }
        bundle = {"format": "recall-ledger.portable/1", "payload": {}, "digest": "b" * 64}
        mapped = {
            "manifest": target_manifest, "commit_token": target_manifest["commit_token"],
            "source_binding": BINDING, "source_digest": bundle["digest"], "evidence_valid": True,
        }
        with patch.object(target, "bundle_context", return_value=mapped) as context:
            with patch.object(target, "validate_context", return_value={"valid": True, "invalid": [], "commit_token": target_manifest["commit_token"]}) as validation:
                rebased = target.rebase_handoff(packet, bundle)
                self.assertTrue(target.verify_handoff(rebased)["evidence_valid"])
                self.assertEqual(validation.call_count, 2)
        self.assertEqual(packet, original)
        self.assertEqual(target.expected_binding, target_binding)
        self.assertEqual(rebased["task"], original["task"])
        self.assertEqual(rebased["evidence"]["binding"], target_binding)
        self.assertEqual(rebased["evidence"]["manifest"], target_manifest)
        self.assertEqual(rebased["rebased_from"]["source_manifest"], MANIFEST)
        self.assertEqual(rebased["rebased_from"]["handoff_digest"], original["digest"])
        self.assertNotEqual(rebased["digest"], original["digest"])
        self.assertEqual(context.call_args.kwargs, {"expected_source": BINDING, "manifest": MANIFEST})
        self.assertEqual(handoff.deserialize_handoff(handoff.serialize_handoff(rebased)), rebased)
        with self.assertRaisesRegex(HandoffError, "handoff_binding_mismatch"):
            target.verify_handoff(original)

    def test_rebase_refuses_unapplied_mismatched_mapping_and_second_check_changes(self):
        packet = self.prepare()
        bundle = {"format": "recall-ledger.portable/1", "payload": {}, "digest": "b" * 64}
        for response in (
            [],
            {"manifest": None},
            {"evidence_valid": True, "manifest": MANIFEST, "source_binding": BINDING, "source_digest": "wrong", "commit_token": MANIFEST["commit_token"]},
            {"evidence_valid": True, "manifest": MANIFEST, "source_binding": dict(BINDING, namespace="other"), "source_digest": bundle["digest"], "commit_token": MANIFEST["commit_token"]},
        ):
            with self.subTest(response=response):
                with patch.object(self.client, "bundle_context", return_value=response):
                    with self.assertRaisesRegex(HandoffError, "invalid_rebase_response"):
                        self.client.rebase_handoff(packet, bundle)
        with patch.object(self.client, "bundle_context", side_effect=MemoryError("migration_conflict", "not imported")):
            with self.assertRaises(MemoryError):
                self.client.rebase_handoff(packet, bundle)
        mapped = {
            "manifest": MANIFEST, "commit_token": MANIFEST["commit_token"],
            "source_binding": BINDING, "source_digest": bundle["digest"], "evidence_valid": True,
        }
        self.validation["valid"] = False
        with patch.object(self.client, "bundle_context", return_value=mapped):
            with self.assertRaisesRegex(HandoffError, "stale_handoff_evidence"):
                self.client.rebase_handoff(packet, bundle)

    def test_rebase_provenance_is_strict_and_covered_by_digest(self):
        packet = self.prepare()
        packet["rebased_from"] = {
            "handoff_digest": packet["digest"], "bundle_digest": "b" * 64,
            "source_binding": BINDING, "source_manifest": MANIFEST,
        }
        self.resign(packet)
        self.assertIn("rebased_from", handoff.inspect_handoff(packet))
        packet["rebased_from"]["source_manifest"] = dict(MANIFEST, records=[])
        with self.assertRaisesRegex(HandoffError, "handoff_digest_mismatch"):
            handoff.inspect_handoff(packet)
        packet["rebased_from"]["extra"] = "unknown"
        self.resign(packet)
        with self.assertRaisesRegex(HandoffError, "invalid_handoff_fields"):
            handoff.inspect_handoff(packet)


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN"), "set RECALL_LEDGER_BIN for a built daemon")
class HandoffDaemonTests(unittest.TestCase):
    def clients(self, daemon):
        chosen = daemon.client.binding()
        return tuple(Client(str(daemon.socket), timeout=2, expected_binding=chosen) for _ in range(2))

    def test_sdk_a_to_b_to_a_revalidates_revision_then_retraction(self):
        with tempfile.TemporaryDirectory(prefix="rl-ho-", dir="/tmp") as temp:
            root = Path(temp).resolve()
            daemon = DaemonFixture(root)
            try:
                a, b = self.clients(daemon)
                source = a.remember("Rust v1 design", idempotency_key="source")
                task_a = dict(TASK, task_id="a-to-b")
                packet_a = a.prepare_handoff(task_a, a.recall("Rust", mode="lexical")["manifest"])
                self.assertTrue(b.verify_handoff(packet_a)["evidence_valid"])
                b.revise(source["id"], expected_revision=1, body="Rust v2 design", idempotency_key="b-revise")
                old = a.verify_handoff(packet_a)
                self.assertFalse(old["evidence_valid"])
                self.assertEqual(old["invalid"][0]["reason"], "revision_changed")
                packet_b = b.prepare_handoff(dict(TASK, progress=["B checked the new design"]), b.recall("Rust", mode="lexical")["manifest"])
                self.assertTrue(a.verify_handoff(packet_b)["evidence_valid"])
                self.assertEqual(packet_b["evidence"]["manifest"]["records"][0]["revision"], 2)
                a.retract(source["id"], expected_revision=2, idempotency_key="a-retract")
                self.assertFalse(b.verify_handoff(packet_b)["evidence_valid"])
                with self.assertRaisesRegex(HandoffError, "stale_handoff_evidence"):
                    a.prepare_handoff(TASK, packet_b["evidence"]["manifest"])
            finally:
                daemon.close()

    def test_actual_daemon_expiry_and_socket_replacement_fail_closed(self):
        with tempfile.TemporaryDirectory(prefix="rl-hx-", dir="/tmp") as temp:
            root = Path(temp).resolve()
            daemon = DaemonFixture(root)
            try:
                a, b = self.clients(daemon)
                expires = int(time.time()) + 2
                a.remember("Expires during handoff", idempotency_key="expiring", valid_until=expires)
                packet = a.prepare_handoff(TASK, a.recall("", mode="lexical")["manifest"])
                self.assertTrue(packet["evidence"]["manifest"]["records"])
                while time.time() <= expires + 0.03:
                    time.sleep(0.02)
                expired = b.verify_handoff(packet)
                self.assertFalse(expired["evidence_valid"])
                self.assertEqual(expired["invalid"][0]["reason"], "ineligible")
            finally:
                daemon.close()
            replacement = DaemonFixture(root, database="replacement.db")
            try:
                with self.assertRaises(MemoryError) as caught:
                    b.verify_handoff(packet)
                self.assertEqual(caught.exception.code, "binding_mismatch")
            finally:
                replacement.close()

    def test_prepare_inspect_verify_cli_uses_only_explicit_json_inputs(self):
        with tempfile.TemporaryDirectory(prefix="rl-hc-", dir="/tmp") as temp:
            root = Path(temp).resolve()
            daemon = DaemonFixture(root)
            try:
                a, _ = self.clients(daemon)
                source = a.remember("CLI evidence", idempotency_key="cli-source")
                paths = {name: root / (name + ".json") for name in ("binding", "task", "manifest", "packet")}
                for name, data in (
                    ("binding", a.expected_binding),
                    ("task", TASK),
                    ("manifest", a.recall("CLI", mode="lexical")["manifest"]),
                ):
                    paths[name].write_text(json.dumps(data), encoding="utf-8")
                command = [sys.executable, "-m", "recall_ledger.handoff"]
                prepared = subprocess.run(
                    command + ["prepare", "--socket", str(daemon.socket), "--binding-file", str(paths["binding"]),
                               "--task-file", str(paths["task"]), "--manifest-file", str(paths["manifest"]), "--output", str(paths["packet"])],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
                )
                self.assertEqual(prepared.stderr, b"")
                self.assertTrue(json.loads(prepared.stdout)["receiver_must_reverify"])
                inspected = subprocess.run(command + ["inspect", "--file", str(paths["packet"])], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
                self.assertFalse(json.loads(inspected.stdout)["evidence_verified"])
                verified_command = command + ["verify", "--socket", str(daemon.socket), "--binding-file", str(paths["binding"]), "--file", str(paths["packet"])]
                verified = subprocess.run(verified_command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
                self.assertTrue(json.loads(verified.stdout)["evidence_valid"])
                a.revise(source["id"], expected_revision=1, body="Changed CLI evidence", idempotency_key="cli-revised")
                stale = subprocess.run(verified_command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(stale.returncode, 2)
                self.assertFalse(json.loads(stale.stdout)["evidence_valid"])
                self.assertFalse(json.loads(stale.stdout)["execution_authorized"])
            finally:
                daemon.close()

    def test_cross_ledger_import_rebase_cli_and_roundtrip_without_rebinding(self):
        with tempfile.TemporaryDirectory(prefix="rl-hp-", dir="/tmp") as temp:
            root = Path(temp).resolve()
            (root / "a").mkdir()
            (root / "b").mkdir()
            daemon_a, daemon_b = DaemonFixture(root / "a", namespace="a"), DaemonFixture(root / "b", namespace="b")
            try:
                a = self.clients(daemon_a)[0]
                b = self.clients(daemon_b)[0]
                original_bindings = (a.expected_binding, b.expected_binding)
                a.remember("Portable Rust design", idempotency_key="portable-source")
                packet_a = a.prepare_handoff(TASK, a.recall("Rust", mode="lexical")["manifest"])
                packet_a_before = copy.deepcopy(packet_a)
                bundle_a = a.bundle_export()
                with self.assertRaises(MemoryError):
                    b.rebase_handoff(packet_a, bundle_a)
                plan = b.bundle_plan(bundle_a, expected_source=a.expected_binding)
                self.assertTrue(plan["can_apply"])
                b.bundle_apply(
                    bundle_a, expected_source=a.expected_binding,
                    reviewed_digest=plan["reviewed_digest"], expected_target_commit=plan["target_commit"], idempotency_key="a-to-b",
                )
                packet_b = b.rebase_handoff(packet_a, bundle_a)
                self.assertTrue(b.verify_handoff(packet_b)["evidence_valid"])
                self.assertEqual(packet_a, packet_a_before)
                packet_file, bundle_file, binding_file, output_file = (root / name for name in ("source.json", "bundle.json", "binding.json", "rebased.json"))
                handoff.save_handoff(packet_a, packet_file)
                bundle_file.write_text(json.dumps(bundle_a), encoding="utf-8")
                binding_file.write_text(json.dumps(b.expected_binding), encoding="utf-8")
                cli = subprocess.run(
                    [sys.executable, "-m", "recall_ledger.handoff", "rebase", "--socket", str(daemon_b.socket),
                     "--binding-file", str(binding_file), "--file", str(packet_file),
                     "--bundle-file", str(bundle_file), "--output", str(output_file)],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
                )
                self.assertEqual(cli.stderr, b"")
                self.assertTrue(json.loads(cli.stdout)["rebased"])
                self.assertTrue(b.verify_handoff(handoff.load_handoff(output_file))["evidence_valid"])
                mapped = packet_b["evidence"]["manifest"]["records"][0]
                b.revise(mapped["id"], expected_revision=mapped["revision"], body="Portable Rust design advanced by B", idempotency_key="b-progress")
                with self.assertRaises(MemoryError):
                    b.rebase_handoff(packet_a, bundle_a)
                latest_b = b.prepare_handoff(dict(TASK, progress=["B revised the shared evidence"]), b.recall("Rust", mode="lexical")["manifest"])
                bundle_b = b.bundle_export()
                reverse_plan = a.bundle_plan(bundle_b, expected_source=b.expected_binding)
                self.assertTrue(reverse_plan["can_apply"], reverse_plan)
                a.bundle_apply(
                    bundle_b, expected_source=b.expected_binding, reviewed_digest=reverse_plan["reviewed_digest"],
                    expected_target_commit=reverse_plan["target_commit"], idempotency_key="b-to-a",
                )
                latest_a = a.rebase_handoff(latest_b, bundle_b)
                self.assertTrue(a.verify_handoff(latest_a)["evidence_valid"])
                self.assertFalse(a.verify_handoff(packet_a)["evidence_valid"])
                self.assertEqual((a.expected_binding, b.expected_binding), original_bindings)
                self.assertEqual(latest_a["rebased_from"]["handoff_digest"], latest_b["digest"])
                self.assertNotIn("rebased_from", latest_a["rebased_from"])
            finally:
                daemon_a.close()
                daemon_b.close()


if __name__ == "__main__":
    unittest.main()
