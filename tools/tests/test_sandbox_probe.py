import importlib.util
import json
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "sandbox_probe.py"
SPEC = importlib.util.spec_from_file_location("sandbox_probe", SCRIPT)
probe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(probe)


class PersistenceTests(unittest.TestCase):
    def test_resume_missing_marker_fails_without_creating_state(self):
        with tempfile.TemporaryDirectory() as work:
            response = probe.sqlite_probe(work, "resume")
            self.assertEqual(response["status"], "fail")
            self.assertEqual(response["observed"]["reason"], "resume_state_missing")
            self.assertEqual(list(Path(work).iterdir()), [])

    def test_sqlite_crash_recovers_then_resume_preserves_identity_and_sequence(self):
        with tempfile.TemporaryDirectory() as work:
            first = probe.sqlite_probe(work, "initial")
            self.assertEqual(first["status"], "pass", first)
            self.assertEqual(first["observed"]["sequence_after"], 1)
            self.assertEqual(first["observed"]["crash_exit"], 73)
            self.assertGreater(first["observed"]["wal_bytes_after_crash"], 0)
            second = probe.sqlite_probe(work, "resume")
            self.assertEqual(second["status"], "pass", second)
            self.assertEqual(second["observed"]["identity"], first["observed"]["identity"])
            self.assertEqual(second["observed"]["sequence_before"], 1)
            self.assertEqual(second["observed"]["sequence_after"], 2)
            self.assertEqual(second["observed"]["uncommitted_rows"], 0)
            self.assertEqual(second["observed"]["integrity_check"], ["ok"])

    def test_initial_cannot_overwrite_an_existing_fixture(self):
        with tempfile.TemporaryDirectory() as work:
            first = probe.sqlite_probe(work, "initial")
            _, marker, database = probe.fixture_paths(work)
            before_marker = marker.read_bytes()
            before_database = database.read_bytes()
            second = probe.sqlite_probe(work, "initial")
            self.assertEqual(second["status"], "fail")
            self.assertEqual(second["observed"]["reason"], "fixture_already_exists")
            self.assertEqual(marker.read_bytes(), before_marker)
            self.assertEqual(database.read_bytes(), before_database)
            self.assertEqual(first["status"], "pass")

    def test_resume_missing_database_does_not_create_an_empty_replacement(self):
        with tempfile.TemporaryDirectory() as work:
            probe.sqlite_probe(work, "initial")
            _, marker, database = probe.fixture_paths(work)
            before_marker = marker.read_bytes()
            database.unlink()
            response = probe.sqlite_probe(work, "resume")
            self.assertEqual(response["status"], "fail")
            self.assertFalse(database.exists())
            self.assertEqual(marker.read_bytes(), before_marker)

    def test_resume_mismatched_identity_rejected_before_modification(self):
        with tempfile.TemporaryDirectory() as work:
            probe.sqlite_probe(work, "initial")
            _, marker, database = probe.fixture_paths(work)
            state = json.loads(marker.read_text())
            state["identity"] = "00000000-0000-4000-8000-000000000000"
            marker.write_text(json.dumps(state))
            response = probe.sqlite_probe(work, "resume")
            self.assertEqual(response["observed"]["reason"], "identity_mismatch")
            with sqlite3.connect(database) as db:
                self.assertEqual(
                    db.execute("SELECT value FROM probe_state WHERE key='durable_sequence'").fetchone(),
                    ("1",),
                )

    def test_resume_rejects_symlink_marker(self):
        with tempfile.TemporaryDirectory() as work:
            probe.sqlite_probe(work, "initial")
            directory, marker, _ = probe.fixture_paths(work)
            moved = directory / "original-marker.json"
            marker.rename(moved)
            marker.symlink_to(moved)
            response = probe.sqlite_probe(work, "resume")
            self.assertEqual(response["observed"]["reason"], "fixture_symlink")

    def test_local_tcp_and_unix_really_exchange_synthetic_bytes(self):
        with tempfile.TemporaryDirectory() as work:
            probe.sqlite_probe(work, "initial")
            self.assertEqual(probe.local_communication_probe(work)["status"], "pass")
            directory, _, _ = probe.fixture_paths(work)
            self.assertFalse(list(directory.glob("socket-*")))


class NetworkTests(unittest.TestCase):
    def test_semantics_never_treat_refused_or_dns_failure_as_policy_proof(self):
        for outcome in ["refused", "dns_error", "dns_timeout", "socket_error", "probe_error"]:
            self.assertEqual(probe.network_semantics(outcome, "deny"), "unknown")
        for outcome in ["timeout", "unreachable", "permission_denied"]:
            self.assertEqual(probe.network_semantics(outcome, "deny"), "pass")
            self.assertEqual(probe.network_semantics(outcome, "allow"), "fail")
        self.assertEqual(probe.network_semantics("connected", "deny"), "fail")
        self.assertEqual(probe.network_semantics("connected", "allow"), "pass")

    def test_positive_canary_connects_but_sends_no_payload(self):
        with socket.socket() as server:
            server.bind(("127.0.0.1", 0))
            server.listen(2)
            value = f"127.0.0.1:{server.getsockname()[1]}"
            allowed = probe.tcp_probe(value, "allow", "positive")
            self.assertEqual(allowed["status"], "pass", allowed)
            peer, _ = server.accept()
            with peer:
                peer.settimeout(1)
                self.assertEqual(peer.recv(1), b"")
            denied = probe.tcp_probe(value, "deny", "negative")
            self.assertEqual(denied["status"], "fail")
            self.assertEqual(denied["observed"]["payload_sent"], False)

    def test_refused_socket_is_unknown_not_pass(self):
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            value = f"127.0.0.1:{reserved.getsockname()[1]}"
        response = probe.tcp_probe(value, "deny", "refusal")
        self.assertEqual(response["observed"]["outcome"], "refused", response)
        self.assertEqual(response["status"], "unknown")

    def test_endpoints_are_validated_and_ipv6_is_unambiguous(self):
        self.assertEqual(probe.endpoint("[fd00:ec2::254]:80"), ("fd00:ec2::254", 80))
        self.assertEqual(probe.endpoint("canary.svc:8080"), ("canary.svc", 8080))
        for value in ["host", ":80", "host:0", "host:65536", "host:no", " space:80", "fd00:ec2::254:80"]:
            with self.assertRaises(ValueError):
                probe.endpoint(value)

    def test_invalid_cli_arguments_are_machine_readable_and_do_not_start_probes(self):
        response = subprocess.run([sys.executable, str(SCRIPT), "--phase", "invalid"],
                                  capture_output=True, text=True, check=False)
        self.assertEqual(response.returncode, 2)
        self.assertEqual(json.loads(response.stdout)["error"]["code"], "invalid_arguments")
        self.assertEqual(response.stderr, "")


class ConfigurationTests(unittest.TestCase):
    def test_mount_parser_chooses_specific_mount_and_not_path_prefix(self):
        mountinfo = (
            "1 0 0:1 / / ro - overlay overlay ro\n"
            "2 1 0:2 / /tmp rw - tmpfs tmpfs rw\n"
            "3 1 8:0 / /work rw - ext4 /dev/device rw\n"
            "4 1 8:0 / /work-old rw - xfs /dev/other rw\n"
        )
        self.assertEqual(probe.mount_for("/tmp/file", mountinfo)["filesystem"], "tmpfs")
        self.assertEqual(probe.mount_for("/work/file", mountinfo)["filesystem"], "ext4")
        self.assertEqual(probe.mount_for("/work-other", mountinfo)["mountpoint"], "/")


if __name__ == "__main__":
    unittest.main()
