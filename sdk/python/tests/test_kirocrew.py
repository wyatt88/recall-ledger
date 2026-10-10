import json
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from recall_ledger import kirocrew
from recall_ledger import host_runtime as host
from test_host_runtime import DaemonFixture, FakeClient, fixture_config


class KiroCrewTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rl-kiro-")
        self.root = Path(self.temp.name).resolve()
        self.fake = FakeClient()
        self.config = fixture_config(self.root, host_name="kirocrew")
        self.config["gateway_binding"] = {"host_instance_id": "gateway-1", "member_id": "member-1", "store_id": "store-1"}
        host.install_files(self.config["state"], self.config, {})
        self.runtime = host.Runtime(self.config, self.fake)

    def tearDown(self):
        self.temp.cleanup()

    def context(self, mode="persistent", **override):
        values = dict(
            host_instance_id="gateway-1", member_id="member-1", store_id="store-1",
            memory_mode=mode, session_key="session-1", agent_id="agent-1",
        )
        values.update(override)
        return kirocrew.FrozenContext(**values)

    def test_gateway_binding_cannot_be_selected_by_payload_or_display_name(self):
        context = kirocrew.FrozenContext("gateway-1", "other-member", "store-1", "persistent", "session", "agent")
        with self.assertRaisesRegex(host.HostError, "gateway_binding_mismatch"):
            kirocrew.GatewayBridge(self.runtime, context)
        bridge = kirocrew.GatewayBridge(self.runtime, self.context())
        with self.assertRaisesRegex(host.HostError, "unsupported_gateway_fields"):
            kirocrew.execute(bridge, {"operation": "capture", "body": "x", "event_id": "e", "store_id": "other"})
        self.assertEqual(self.fake.calls, [])

    def test_incognito_reads_without_captures_and_temporary_does_nothing(self):
        for mode in ("incognito", "temporary"):
            bridge = kirocrew.GatewayBridge(self.runtime, self.context(mode))
            result = bridge.capture(event_id="event-1", body="Private message")
            self.assertFalse(result["captured"])
            bridge.flush()
            read = bridge.refresh(query="Rust", manifest=None)
            self.assertEqual(read["available"], mode == "incognito")
        self.assertEqual([method for method, _ in self.fake.calls], ["recall"])
        self.assertFalse((self.runtime.state / "sessions").exists())
        self.assertFalse((self.runtime.state / "outbox").exists())

    def test_context_file_must_be_private_and_is_not_taken_from_stdin(self):
        path = self.root / "context.json"
        path.write_text(json.dumps(self.context().__dict__))
        path.chmod(0o644)
        with self.assertRaisesRegex(host.HostError, "owner_private"):
            kirocrew.load_context(path)
        path.chmod(0o600)
        self.assertEqual(kirocrew.load_context(path), self.context())

    def test_install_only_generates_bound_example_and_own_state(self):
        state = self.root / "new-install"
        with patch.object(host, "Client", return_value=self.fake):
            result = kirocrew.install(str(state), str(self.root / "s"), host_instance_id="gateway-1", member_id="member-1", store_id="store-1", binary="/chosen/recall-ledger")
        self.assertFalse(result["gateway_wired"])
        example = host.read_json(Path(result["mcp_example"]))
        args = example["mcpServers"]["recall-ledger"]["args"]
        self.assertIn("--binding-file", args)
        self.assertEqual(host.read_json(state / "binding.json"), self.fake.expected)
        self.assertFalse((self.root / ".kiro").exists())

    def test_new_mcp_example_created_during_generation_is_not_overwritten(self):
        state = self.root / "racing-install"
        original_install = host.install_files
        new_bytes = host.encode({"user_created": "keep this"})

        def edit_before_lock(state_arg, config, updates, *, expected_before):
            host.directory(state)
            (state / "mcp.example.json").write_bytes(new_bytes)
            return original_install(state_arg, config, updates, expected_before=expected_before)

        with patch.object(host, "Client", return_value=self.fake):
            with patch.object(host, "install_files", side_effect=edit_before_lock):
                with self.assertRaisesRegex(host.HostError, "configuration_changed"):
                    kirocrew.install(
                        str(state), str(self.root / "s"),
                        host_instance_id="gateway-1", member_id="member-1",
                        store_id="store-1", binary="/chosen/recall-ledger",
                    )
        self.assertEqual((state / "mcp.example.json").read_bytes(), new_bytes)
        for name in ("binding.json", "config.json", "installation.json"):
            self.assertFalse((state / name).exists())


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN"), "set RECALL_LEDGER_BIN for a built daemon")
class KiroCrewDaemonTests(unittest.TestCase):
    def test_real_daemon_gateway_capture_privacy_read_and_refresh(self):
        with tempfile.TemporaryDirectory(prefix="rl-kr-", dir="/tmp") as temp:
            root = Path(temp).resolve()
            daemon = DaemonFixture(root)
            try:
                state = root / "bridge"
                kirocrew.install(str(state), str(daemon.socket), host_instance_id="g", member_id="m", store_id="st", binary=os.environ["RECALL_LEDGER_BIN"])
                runtime = host.Runtime.load(state)
                bridge = kirocrew.GatewayBridge(runtime, kirocrew.FrozenContext("g", "m", "st", "persistent", "session", "agent"))
                bridge.capture(event_id="gateway:event:1", body="Rust shared memory")
                bridge.flush()
                self.assertEqual(bridge.read(query="Rust")["items"], [])
                candidate = daemon.client.candidate_list()["candidates"][0]["candidate_id"]
                accepted = daemon.client.candidate_resolve(candidate, "accept", idempotency_key="trusted-review")
                current = bridge.read(query="Rust")
                self.assertEqual(current["items"][0]["body"], "Rust shared memory")
                context_file = root / "execution-context.json"
                context_file.write_bytes(host.encode(bridge.context.__dict__))
                context_file.chmod(0o600)
                cli = subprocess.run(
                    [sys.executable, "-m", "recall_ledger.kirocrew", "call", "--state", str(state), "--context-file", str(context_file)],
                    input=host.encode({"operation": "read", "query": "Rust"}),
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
                )
                self.assertEqual(cli.stderr, b"")
                self.assertEqual(json.loads(cli.stdout)["items"][0]["body"], "Rust shared memory")
                helper_path = Path(__file__).resolve().parents[3] / "integrations" / "kirocrew" / "gateway_bridge.py"
                spec = importlib.util.spec_from_file_location("gateway_bridge_fixture", helper_path)
                helper = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(helper)
                dispatch = helper.before_model_dispatch(state=state, context=bridge.context, query="Rust", previous_manifest=current["manifest"])
                self.assertIn("Rust shared memory", dispatch["context"])
                for mode in ("incognito", "temporary"):
                    private = kirocrew.GatewayBridge(runtime, kirocrew.FrozenContext("g", "m", "st", mode, "private-session", "agent"))
                    private.capture(event_id="private:" + mode, body="Never record")
                    private.flush()
                    response = private.read(query="Rust")
                    self.assertEqual(response["available"], mode == "incognito")
                self.assertEqual(daemon.client.candidate_list()["total"], 1)
                daemon.client.retract(accepted["record_id"], expected_revision=1, idempotency_key="withdraw")
                fresh = bridge.refresh(query="Rust", manifest=current["manifest"])
                self.assertFalse(fresh["previous_context_valid"])
                self.assertEqual(fresh["items"], [])
            finally:
                daemon.close()


if __name__ == "__main__":
    unittest.main()
