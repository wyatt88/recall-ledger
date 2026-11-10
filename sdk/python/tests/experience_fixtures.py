"""Disposable real daemon fixtures for the experience/commitment modules."""

import copy
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

from recall_ledger.client import Client
from recall_ledger.environment import capture_environment
from recall_ledger.experience import RECEIPT_SCHEMA

BINARY = os.environ.get("RECALL_LEDGER_BIN")
NOW = "2026-11-09T10:00:00Z"
LATER = "2026-11-09T11:00:00Z"


class DaemonCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rl-experience-", dir="/tmp")
        self.root = Path(self.temp.name).resolve()
        self.processes = {}
        self.addCleanup(self.cleanup)
        self.client = self.start("experiences")
        self.trusted = {}
        (self.root / "requirements.lock").write_text("dependency==1\n", encoding="utf-8")

    def cleanup(self):
        self.stop()
        self.temp.cleanup()

    def stop(self):
        for process in self.processes.values():
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            process.stderr.close()
        self.processes.clear()

    def start(self, namespace):
        # The core owns one daemon lock per DB; switch namespaces sequentially
        # against that same database to test actual namespace isolation.
        self.stop()
        socket = self.root / (namespace + ".sock")
        socket.unlink(missing_ok=True)
        process = subprocess.Popen(
            [str(Path(BINARY).resolve()), "--db", str(self.root / "db"),
             "--namespace", namespace, "serve", "--socket", str(socket), "--no-worker"],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
        )
        self.processes[namespace] = process
        deadline = time.monotonic() + 10
        while True:
            if process.poll() is not None:
                self.fail("Daemon exited: " + process.stderr.read().decode())
            if time.monotonic() >= deadline:
                self.fail("Daemon did not start")
            try:
                probe = Client(str(socket), timeout=1)
                binding = probe.binding()
                return Client(str(socket), timeout=5, expected_binding=binding)
            except Exception:
                time.sleep(0.01)

    def restart(self, namespace="experiences"):
        process = self.processes.pop(namespace)
        process.terminate()
        process.wait(timeout=5)
        process.stderr.close()
        (self.root / (namespace + ".sock")).unlink(missing_ok=True)
        return self.start(namespace)

    def snapshot(self, at=NOW):
        return capture_environment(
            roots={"project": self.root},
            files=[{"root": "project", "path": "requirements.lock"}],
            tool_versions={"test-runner": "1.0"}, observed_at=at,
        )

    def receipt(self, name="run-1", status="succeeded"):
        value = {"schema": RECEIPT_SCHEMA, "receipt_id": name, "host_id": "trusted-test-host",
                 "tool": "test-runner", "tool_version": "1.0", "status": status,
                 "input_sha256": "a" * 64, "output_sha256": "b" * 64,
                 "started_at": "2026-11-09T09:59:00Z", "completed_at": NOW,
                 "exit_code": 0 if status == "succeeded" else 1,
                 "checks": [{"name": "test-suite", "passed": status == "succeeded"}]}
        self.trusted[name] = copy.deepcopy(value)
        return value

    def verify(self, receipt):
        return self.trusted.get(receipt["receipt_id"]) == receipt
