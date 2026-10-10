import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from recall_ledger import codex
from recall_ledger import host_runtime as host
from test_host_runtime import DaemonFixture, FakeClient


class CodexTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rl-codex-")
        self.root = Path(self.temp.name).resolve()
        self.fake = FakeClient()

    def tearDown(self):
        self.temp.cleanup()

    def install(self, mode="persistent"):
        with patch.object(host, "Client", return_value=self.fake):
            codex.install(str(self.root), str(self.root / "s"), memory_mode=mode)
        config = host.read_json(self.root / ".codex" / "recall-ledger" / "config.json")
        return host.Runtime(config, self.fake)

    def event(self, name="UserPromptSubmit", turn="turn-1", body="Rust memory"):
        value = {"hook_event_name": name, "session_id": "session", "cwd": str(self.root)}
        if name == "SessionStart":
            value["source"] = "compact"
        else:
            value["turn_id"] = turn
            value["prompt" if name == "UserPromptSubmit" else "last_assistant_message"] = body
        return value

    @unittest.skipUnless(sys.version_info >= (3, 11), "Codex TOML installation uses stdlib tomllib")
    def test_install_preserves_toml_hooks_and_reports_trust_as_unverified(self):
        import tomllib

        config_dir = self.root / ".codex"
        config_dir.mkdir()
        original = b'# Preserve this comment\nmodel = "test-model"\n[memories]\nuse_memories = true\nextract_model = "unchanged"\n'
        (config_dir / "config.toml").write_bytes(original)
        old_hooks = {"description": "user hooks", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "user-stop"}]}]}}
        (config_dir / "hooks.json").write_text(json.dumps(old_hooks))
        with patch.object(host, "Client", return_value=self.fake):
            result = codex.install(str(self.root), str(self.root / "s"))
        self.assertFalse(result["host_trusted"])
        self.assertFalse(result["effective_hooks_verified"])
        data = (config_dir / "config.toml").read_text()
        self.assertIn("# Preserve this comment", data)
        self.assertEqual(tomllib.loads(data), {
            "model": "test-model", "memories": {"use_memories": False, "generate_memories": False, "extract_model": "unchanged"},
        })
        hooks = host.read_json(config_dir / "hooks.json")
        self.assertEqual(hooks["hooks"]["Stop"][0], old_hooks["hooks"]["Stop"][0])
        self.assertEqual(hooks["hooks"]["SessionStart"][0]["matcher"], "^compact$")
        self.assertEqual(hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["additionalContextLimit"], 0)

    @unittest.skipUnless(sys.version_info >= (3, 11), "requires tomllib")
    def test_edit_between_generation_and_install_lock_is_preserved_without_partial_patch(self):
        config_dir = self.root / ".codex"
        config_dir.mkdir()
        hooks_path = config_dir / "hooks.json"
        native_path = config_dir / "config.toml"
        state = config_dir / "recall-ledger"
        hooks_before = host.encode({"hooks": {}})
        native_before = b'model = "original"\n'
        original_install = host.install_files
        for changed, new_bytes in (
            (hooks_path, host.encode({"hooks": {}, "user_edit": "keep"})),
            (native_path, b'model = "new-user-choice"\n'),
        ):
            with self.subTest(changed=changed.name):
                hooks_path.write_bytes(hooks_before)
                native_path.write_bytes(native_before)

                def edit_before_lock(state_arg, config, updates, *, expected_before):
                    self.assertEqual(expected_before[hooks_path], hooks_before)
                    self.assertEqual(expected_before[native_path], native_before)
                    changed.write_bytes(new_bytes)
                    return original_install(state_arg, config, updates, expected_before=expected_before)

                with patch.object(host, "Client", return_value=self.fake):
                    with patch.object(host, "install_files", side_effect=edit_before_lock):
                        with self.assertRaisesRegex(host.HostError, "configuration_changed"):
                            codex.install(str(self.root), str(self.root / "s"))
                self.assertEqual(hooks_path.read_bytes(), new_bytes if changed == hooks_path else hooks_before)
                self.assertEqual(native_path.read_bytes(), new_bytes if changed == native_path else native_before)
                for name in ("installation.json", "config.json", "binding.json"):
                    self.assertFalse((state / name).exists())

    @unittest.skipUnless(sys.version_info >= (3, 11), "requires tomllib")
    def test_toml_semantic_guard_preserves_other_keys_and_refuses_ambiguous_shape(self):
        import tomllib

        cases = [
            b'memories.use_memories = true\nmodel = "unchanged"\n',
            b'[memories]\ngenerate_memories = true # old choice\nuse_memories = true\n[other]\nuse_memories = true\n',
        ]
        for raw in cases:
            expected = copy.deepcopy(tomllib.loads(raw.decode()))
            expected["memories"].update(generate_memories=False, use_memories=False)
            self.assertEqual(tomllib.loads(codex.disable_native(raw).decode()), expected)
        with self.assertRaises(host.HostError):
            codex.disable_native(b'memories = { use_memories = true }\n')

    @unittest.skipUnless(sys.version_info >= (3, 11), "requires tomllib")
    def test_turn_identity_distinguishes_same_text_and_conflicts_on_changed_payload(self):
        runtime = self.install()
        codex.run_hook(runtime, self.event(turn="turn-1"))
        codex.run_hook(runtime, self.event(turn="turn-2"))
        codex.run_hook(runtime, self.event(turn="turn-2"))
        with self.assertRaisesRegex(host.HostError, "event_conflict"):
            codex.run_hook(runtime, self.event(turn="turn-2", body="different same event"))
        submits = [params for method, params in self.fake.calls if method == "candidate_submit"]
        self.assertNotEqual(submits[0]["event_id"], submits[1]["event_id"])
        self.assertEqual(submits[1], submits[2])
        child = self.event(turn="turn-1")
        child["agent_id"] = "child-1"
        codex.run_hook(runtime, child)
        self.assertNotEqual(submits[0]["event_id"], self.fake.calls[-2][1].get("event_id"))

    @unittest.skipUnless(sys.version_info >= (3, 11), "requires tomllib")
    def test_compact_refresh_and_stop_do_not_request_continuation(self):
        runtime = self.install()
        self.fake.body = "记忆数据🧠" * 150
        prompt = codex.run_hook(runtime, self.event())
        data = host.bounded_output(prompt, 4096)
        self.assertLessEqual(len(data), 4096)
        compact = codex.run_hook(runtime, self.event("SessionStart"))
        self.assertEqual(compact["hookSpecificOutput"]["hookEventName"], "SessionStart")
        self.assertEqual([p for m, p in self.fake.calls if m == "recall"][-1]["query"], "")
        stopped = codex.run_hook(runtime, self.event("Stop"))
        self.assertEqual(stopped, {})
        self.assertNotIn("decision", stopped)
        before = len(self.fake.calls)
        continued = dict(self.event("Stop", body="Continued assistant output"), stop_hook_active=True)
        self.assertEqual(codex.run_hook(runtime, continued), {})
        self.assertEqual(len(self.fake.calls), before)
        ignored = dict(self.event("SessionStart"), source="resume")
        self.assertEqual(codex.run_hook(runtime, ignored), {})

    @unittest.skipUnless(sys.version_info >= (3, 11), "requires tomllib")
    def test_temporary_hook_leaves_no_candidate_or_session_content(self):
        runtime = self.install("temporary")
        self.fake.calls.clear()
        output = codex.run_hook(runtime, self.event(body="Do not persist this private text"))
        self.assertIn("privacy_temporary", output["hookSpecificOutput"]["additionalContext"])
        self.assertEqual(self.fake.calls, [])
        self.assertFalse((runtime.state / "events").exists())
        self.assertFalse((runtime.state / "outbox").exists())
        self.assertFalse((runtime.state / "sessions").exists())

    @unittest.skipUnless(sys.version_info >= (3, 11), "requires tomllib")
    def test_uninstall_preserves_native_disable_and_old_hook_is_inert(self):
        import tomllib

        runtime = self.install()
        with patch.object(host.Runtime, "load", return_value=runtime):
            result = codex.uninstall(str(self.root))
        self.assertTrue(result["configuration_removed"])
        self.assertFalse((self.root / ".codex" / "hooks.json").exists())
        config = tomllib.loads((self.root / ".codex" / "config.toml").read_text())
        self.assertEqual(config["memories"], {"generate_memories": False, "use_memories": False})
        self.fake.calls.clear()
        self.assertEqual(codex.run_hook(runtime, self.event("Stop")), {})
        self.assertEqual(self.fake.calls, [])


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN") and sys.version_info >= (3, 11), "set RECALL_LEDGER_BIN and use Python 3.11+")
class CodexDaemonTests(unittest.TestCase):
    def test_actual_daemon_cli_pending_accept_compact_revision_and_retract(self):
        with tempfile.TemporaryDirectory(prefix="rl-cdx-", dir="/tmp") as temp:
            root = Path(temp).resolve()
            daemon = DaemonFixture(root)
            try:
                codex.install(str(root), str(daemon.socket))
                hooks_before_alias = (root / ".codex" / "hooks.json").read_bytes()
                alias = subprocess.run(
                    [sys.executable, "-m", "recall_ledger.chatgpt", "install", "--project", str(root), "--socket", str(daemon.socket)],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
                )
                self.assertTrue(json.loads(alias.stdout)["already_installed"])
                self.assertEqual((root / ".codex" / "hooks.json").read_bytes(), hooks_before_alias)
                runtime = host.Runtime.load(root / ".codex" / "recall-ledger")
                event = {"hook_event_name": "UserPromptSubmit", "session_id": "session", "turn_id": "turn-1", "cwd": str(root), "prompt": "Rust project language"}
                output = subprocess.run(
                    [sys.executable, "-m", "recall_ledger.codex", "hook", "--project", str(root)],
                    input=host.encode(event), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
                )
                self.assertEqual(output.stderr, b"")
                self.assertLessEqual(len(output.stdout), 4096)
                self.assertEqual(json.loads(output.stdout)["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit")
                self.assertEqual(daemon.client.recall("Rust", mode="lexical")["items"], [])
                candidate = daemon.client.candidate_list()["candidates"][0]["candidate_id"]
                record = daemon.client.candidate_resolve(candidate, "accept", idempotency_key="reviewed")["record_id"]
                compact = {"hook_event_name": "SessionStart", "session_id": "session", "cwd": str(root), "source": "compact"}
                first = codex.run_hook(runtime, compact)
                self.assertIn("Rust project language", first["hookSpecificOutput"]["additionalContext"])
                daemon.client.revise(record, expected_revision=1, body="Rust revised project", idempotency_key="revised")
                self.assertIn("Rust revised project", codex.run_hook(runtime, compact)["hookSpecificOutput"]["additionalContext"])
                daemon.client.retract(record, expected_revision=2, idempotency_key="withdrawn")
                self.assertNotIn("Rust revised project", codex.run_hook(runtime, compact)["hookSpecificOutput"]["additionalContext"])
                codex.uninstall(str(root))
            finally:
                daemon.close()


if __name__ == "__main__":
    unittest.main()
