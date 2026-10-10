"""Host-envelope fixtures and isolated real-daemon tests; no model invocation."""

import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from recall_ledger import Client, MemoryError, TransportError
from recall_ledger import claude_code as adapter

BINDING = {
    "namespace": "fixture",
    "history_id": "12345678-1234-5678-9234-567812345678",
}
OTHER_BINDING = {
    "namespace": "fixture",
    "history_id": "87654321-4321-8765-9234-567812345678",
}


class FakeClient:
    def __init__(self):
        self.timeout = 10
        self.calls = []
        self.candidates = {}
        self.token = "history:1"
        self.body = "Accepted canonical memory"
        self.revision = 1
        self.fail_submit = False
        self.fail_recall = False
        self.history_mismatch = False

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "candidate_submit":
            self.candidates[params["event_id"]] = copy.deepcopy(params)
            if self.fail_submit:
                raise TransportError("response lost", outcome_unknown=True)
            return {"candidate_id": params["event_id"], "state": "pending", "commit_token": self.token}
        if method == "recall":
            if self.fail_recall:
                raise TransportError("offline", outcome_unknown=False)
            if self.history_mismatch and "minimum_commit" in params:
                raise MemoryError("commit_history_mismatch", "restored history")
            return {
                "items": [{"id": "record-1", "revision": self.revision, "body": self.body, "trust_boundary": "retrieved_data"}],
                "commit_token": self.token,
                "manifest": {
                    "schema_version": "recall-ledger.context/1", "namespace": "fixture",
                    "commit_token": self.token, "records": [{"id": "record-1", "revision": self.revision}],
                },
                "coverage": {"mode": "lexical"},
            }
        if method == "export":
            return {"records": [{"id": "record-1", "revision": self.revision, "body": self.body}], "commit_token": self.token}
        raise AssertionError(method)

    def candidate_list(self, **params):
        return {"candidates": [{"candidate_id": key, "state": "pending", "body_included": False} for key in self.candidates], "truncated": False}


class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rl-claude-")
        self.root = Path(self.temp.name).resolve()
        self.project = self.root / "isolated project"
        self.project.mkdir()
        self.socket = str(self.root / "s")
        self.settings = self.project / ".claude" / "settings.local.json"
        self.fake = FakeClient()
        # Fixture installs discover metadata only; all data operations remain
        # the explicitly injected FakeClient below.
        binding_patch = patch.object(Client, "binding", return_value=dict(BINDING))
        self.binding_lookup = binding_patch.start()
        self.addCleanup(binding_patch.stop)

    def tearDown(self):
        self.temp.cleanup()

    def install(self, **kwargs):
        adapter.install(str(self.project), self.socket, **kwargs)
        return adapter.load_config(str(self.project))

    def event(self, name="UserPromptSubmit", body="Rust memory adapter"):
        event = {
            "session_id": "fixture-session", "hook_event_name": name,
            "cwd": str(self.project), "transcript_path": str(self.root / "transcript.jsonl"),
        }
        event["prompt" if name == "UserPromptSubmit" else "last_assistant_message"] = body
        if name == "Stop":
            event["stop_hook_active"] = False
        return event

    def packet(self, output):
        text = output["hookSpecificOutput"]["additionalContext"]
        self.assertTrue(text.startswith(adapter.CONTEXT_PREFIX))
        return json.loads(text[len(adapter.CONTEXT_PREFIX):])

    def test_install_preserves_rules_custom_hooks_and_does_not_edit_native_files(self):
        self.settings.parent.mkdir()
        original = {
            "permissions": {"allow": ["Read"]}, "autoMemoryEnabled": True,
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "user-hook"}]}]},
        }
        self.settings.write_text(json.dumps(original))
        native = self.project / "CLAUDE.md"
        native.write_text("Existing user policy", encoding="utf-8")
        self.install()
        installed = json.loads(self.settings.read_text())
        self.assertFalse(installed["autoMemoryEnabled"])
        self.assertEqual(installed["permissions"], original["permissions"])
        self.assertEqual(installed["hooks"]["Stop"][0], original["hooks"]["Stop"][0])
        self.assertEqual(len(installed["hooks"]["Stop"]), 2)
        command = installed["hooks"]["UserPromptSubmit"][0]["hooks"][0]
        self.assertEqual(command["type"], "command")
        self.assertIn("recall_ledger.claude_code", command["command"])
        self.assertEqual(native.read_text(), "Existing user policy")
        self.assertEqual(self.settings.stat().st_mode & 0o777, 0o600)
        result = adapter.install(str(self.project), self.socket)
        self.assertTrue(result["already_installed"])

    def test_interrupted_install_recovers_from_prepared_receipt(self):
        self.install()
        self.settings.unlink()  # Simulate interruption before the settings write.
        result = adapter.install(str(self.project), self.socket)
        self.assertTrue(result["recovered_install"])
        self.assertFalse(json.loads(self.settings.read_text())["autoMemoryEnabled"])

    def test_every_prompt_fresh_recall_and_revision_manifest_without_cached_bodies(self):
        config = self.install()
        first = self.packet(adapter.run_hook(config, self.event(), self.fake))
        self.assertEqual(first["items"][0]["revision"], 1)
        self.fake.body, self.fake.revision, self.fake.token = "New accepted memory", 2, "history:4"
        second = self.packet(adapter.run_hook(config, self.event(body="Updated Rust memory"), self.fake))
        calls = [params for method, params in self.fake.calls if method == "recall"]
        self.assertNotIn("minimum_commit", calls[0])
        self.assertEqual(calls[1]["minimum_commit"], "history:1")
        self.assertEqual(second["manifest"]["records"][0]["revision"], 2)
        self.assertEqual(second["items"][0]["body"], "New accepted memory")
        receipt = adapter.read_json(adapter.session_path(config, "fixture-session"))
        self.assertNotIn("New accepted memory", json.dumps(receipt))
        self.assertEqual(receipt["manifest"], second["manifest"])

    def test_hooks_capture_both_origins_only_as_pending_proposals(self):
        config = self.install()
        self.packet(adapter.run_hook(config, self.event(body="用户讨论，不等于事实"), self.fake))
        output = adapter.run_hook(config, self.event("Stop", "模型提议"), self.fake)
        self.assertEqual(output, {})
        self.assertEqual({p["origin"] for p in self.fake.candidates.values()}, {"user_message", "model_inference"})
        self.assertFalse(any(method in ("remember", "candidate_resolve") for method, _ in self.fake.calls))

    def test_lost_response_retry_uses_same_event_even_after_transcript_append(self):
        config = self.install()
        event = self.event(body="Stable proposal")
        transcript = Path(event["transcript_path"])
        transcript.write_text(json.dumps({"uuid": "previous-uuid", "sessionId": "fixture-session"}) + "\n")
        self.fake.fail_submit = True
        first = self.packet(adapter.run_hook(config, event, self.fake))
        transcript.write_text(json.dumps({
            "uuid": "new-message-uuid", "sessionId": "fixture-session", "message": {"content": "Stable proposal"},
        }) + "\n")
        self.fake.fail_submit = False
        second = self.packet(adapter.run_hook(config, event, self.fake))
        self.assertEqual(first["capture"]["event_id"], second["capture"]["event_id"])
        submits = [params for method, params in self.fake.calls if method == "candidate_submit"]
        self.assertEqual(submits[0], submits[1])
        self.assertEqual(len(self.fake.candidates), 1)
        self.assertEqual(second["capture"]["queued"], 0)

    def test_history_mismatch_keeps_barrier_and_never_retries_without_it(self):
        config = self.install()
        adapter.run_hook(config, self.event(), self.fake)
        receipt_path = adapter.session_path(config, "fixture-session")
        before = receipt_path.read_bytes()
        self.fake.history_mismatch, self.fake.token = True, "restored:2"
        output = adapter.run_hook(config, self.event(body="Next prompt"), self.fake)
        recalls = [params for method, params in self.fake.calls if method == "recall"]
        self.assertEqual(len(recalls), 2)
        self.assertEqual(recalls[-1]["minimum_commit"], "history:1")
        context = output["hookSpecificOutput"]["additionalContext"]
        self.assertIn("commit_history_mismatch", context)
        self.assertNotIn(self.fake.body, context)
        self.assertEqual(receipt_path.read_bytes(), before)

    def test_offline_fails_open_with_outbox_and_no_cached_memory(self):
        config = self.install()
        adapter.run_hook(config, self.event(body="First"), self.fake)
        self.fake.fail_submit = self.fake.fail_recall = True
        output = adapter.run_hook(config, self.event(body="Offline proposal"), self.fake)
        context = output["hookSpecificOutput"]["additionalContext"]
        self.assertIn("No cached memory body", context)
        self.assertNotIn(self.fake.body, context)
        outbox = list((Path(config["state"]) / "outbox").glob("*.json"))
        self.assertEqual(len(outbox), 1)
        self.assertEqual(adapter.read_json(outbox[0])["params"]["body"], "Offline proposal")
        self.assertEqual(adapter.read_json(outbox[0])["expected_binding"], BINDING)

    def test_install_pins_identity_and_every_adapter_client_uses_it(self):
        config = self.install()
        self.assertEqual(config["expected_binding"], BINDING)
        self.assertEqual(adapter.client_for(config).expected_binding, BINDING)
        with self.assertRaisesRegex(adapter.AdapterError, "client_binding_does_not_match"):
            adapter.run_hook(config, self.event(), Client(self.socket))
        self.assertFalse(self.fake.calls)

    def test_failed_install_binding_lookup_preserves_settings_and_does_not_install(self):
        self.settings.parent.mkdir()
        before = b'{"custom": true}\n'
        self.settings.write_bytes(before)
        with patch.object(Client, "binding", side_effect=TransportError("offline", False)):
            with self.assertRaises(TransportError):
                adapter.install(str(self.project), self.socket)
        self.assertEqual(self.settings.read_bytes(), before)
        self.assertFalse((self.settings.parent / "recall-ledger" / "config.json").exists())
        self.assertFalse((self.settings.parent / "recall-ledger" / "installation.json").exists())

    def test_active_install_and_preinstall_selection_do_not_implicitly_rebind(self):
        for installed in (False, True):
            with self.subTest(installed=installed):
                if installed:
                    self.install()
                else:
                    adapter.load_config(str(self.project), self.socket)
                before = (self.settings.parent / "recall-ledger" / "config.json").read_bytes()
                seen = []

                def checked_binding(client):
                    seen.append(client.expected_binding)
                    raise MemoryError("binding_mismatch", "fixture")

                with patch.object(Client, "binding", checked_binding):
                    with self.assertRaises(MemoryError):
                        adapter.install(str(self.project), self.socket)
                self.assertEqual(seen, [BINDING])
                self.assertEqual((self.settings.parent / "recall-ledger" / "config.json").read_bytes(), before)

    def test_concurrent_first_socket_commands_share_one_persisted_binding(self):
        entered = threading.Event()
        release = threading.Event()
        results, errors, discoveries = [], [], []

        def discover(client):
            self.assertIsNone(client.expected_binding)
            discoveries.append(client.socket_path)
            entered.set()
            self.assertTrue(release.wait(2))
            return dict(BINDING)

        def load():
            try:
                results.append(adapter.load_config(str(self.project), self.socket))
            except Exception as exc:
                errors.append(exc)

        with patch.object(Client, "binding", discover):
            first = threading.Thread(target=load)
            second = threading.Thread(target=load)
            first.start()
            self.assertTrue(entered.wait(2))
            second.start()
            release.set()
            first.join(3)
            second.join(3)
        self.assertFalse(first.is_alive())
        self.assertFalse(second.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(discoveries, [self.socket])
        self.assertEqual(len(results), 2)
        self.assertEqual(results[0], results[1])
        self.assertEqual(results[0]["expected_binding"], BINDING)
        self.assertFalse(self.settings.exists())

    def test_legacy_config_refuses_load_and_active_install_requires_explicit_uninstall(self):
        config = self.install()
        config.pop("expected_binding")
        path = Path(config["state"]) / "config.json"
        adapter.atomic_write(path, adapter.encode(config))
        self.binding_lookup.reset_mock()
        with self.assertRaisesRegex(adapter.AdapterError, "binding_missing.*reinstall"):
            adapter.load_config(str(self.project))
        with self.assertRaisesRegex(adapter.AdapterError, "binding_missing.*reinstall"):
            adapter.install(str(self.project), self.socket)
        self.binding_lookup.assert_not_called()
        self.assertFalse(self.fake.calls)

    def test_legacy_uninstall_is_local_only_and_reinstall_never_retargets_old_queue(self):
        config = self.install()
        adapter.enqueue(config, {
            "event_id": "legacy-event", "body": "Keep this proposal", "origin": "user_message",
            "source": "fixture", "idempotency_key": "legacy-event",
        })
        outbox = next((Path(config["state"]) / "outbox").glob("*.json"))
        legacy_entry = adapter.read_json(outbox)
        legacy_entry.pop("expected_binding")
        adapter.atomic_write(outbox, adapter.encode(legacy_entry))
        config.pop("expected_binding")
        adapter.atomic_write(Path(config["state"]) / "config.json", adapter.encode(config))
        before = outbox.read_bytes()
        with patch.object(adapter, "client_for", side_effect=AssertionError("unbound daemon read")):
            result = adapter.uninstall(str(self.project))
        fallback = adapter.read_json(Path(result["fallback"]))
        self.assertFalse(fallback["binding_verified"])
        self.assertIsNone(fallback["snapshot"])
        self.assertIn("No daemon data was read", fallback["warning"])
        self.assertEqual(outbox.read_bytes(), before)
        with patch.object(Client, "binding", return_value=dict(OTHER_BINDING)):
            adapter.install(str(self.project), self.socket)
        rebound = adapter.load_config(str(self.project))
        self.assertEqual(rebound["expected_binding"], OTHER_BINDING)
        result = adapter.flush_outbox(rebound, self.fake, deadline=time.monotonic() + 1)
        self.assertEqual(result["delivered"], 0)
        self.assertIn("outbox_binding_missing", result["error"])
        self.assertEqual(outbox.read_bytes(), before)
        self.assertFalse(self.fake.calls)

    def test_foreign_or_legacy_outbox_entries_are_preserved_without_any_send(self):
        config = self.install()
        for binding in (None, OTHER_BINDING, dict(BINDING, namespace="other")):
            with self.subTest(binding=binding):
                adapter.enqueue(config, {
                    "event_id": "queued", "body": "Original proposal", "origin": "user_message",
                    "source": "fixture", "idempotency_key": "queued",
                })
                path = next((Path(config["state"]) / "outbox").glob("*.json"))
                entry = adapter.read_json(path)
                if binding is None:
                    entry.pop("expected_binding")
                else:
                    entry["expected_binding"] = binding
                adapter.atomic_write(path, adapter.encode(entry))
                before = path.read_bytes()
                result = adapter.flush_outbox(config, self.fake, deadline=time.monotonic() + 1)
                self.assertEqual(result["delivered"], 0)
                self.assertEqual(result["queued"], 1)
                self.assertEqual(result["quarantined"], 0)
                self.assertIn("outbox_binding_", result["error"])
                self.assertEqual(path.read_bytes(), before)
                with self.assertRaisesRegex(adapter.AdapterError, "outbox_binding_"):
                    adapter.enqueue(config, entry["params"])
                self.assertFalse(self.fake.calls)
                path.unlink()

    def test_binding_mismatch_preserves_queue_and_injects_no_other_database_body(self):
        config = self.install()
        with patch.object(self.fake, "call", side_effect=MemoryError("binding_mismatch", "fixture", False)) as call:
            output = adapter.run_hook(config, self.event(), self.fake)
        self.assertEqual([args[0][0] for args in call.call_args_list], ["candidate_submit", "recall"])
        self.assertIn("binding_mismatch", output["hookSpecificOutput"]["additionalContext"])
        self.assertNotIn(self.fake.body, output["hookSpecificOutput"]["additionalContext"])
        self.assertEqual(len(list((Path(config["state"]) / "outbox").glob("*.json"))), 1)
        self.assertEqual(adapter.dead_letters(config)["total"], 0)

    def test_dead_letter_requeue_cannot_stamp_current_binding_on_old_payload(self):
        config = self.install()
        entry = adapter.digest(b"dead-letter-event")
        path = adapter.dead_letter_path(config, entry)
        params = {
            "event_id": "dead-letter-event", "body": "Original proposal", "origin": "user_message",
            "source": "fixture", "idempotency_key": "dead-letter-event",
        }
        for binding in (None, OTHER_BINDING):
            with self.subTest(binding=binding):
                item = {"schema": adapter.SCHEMA, "params": params, "error": {"code": "invalid_request"}}
                if binding is not None:
                    item["expected_binding"] = binding
                adapter.atomic_write(path, adapter.encode(item))
                before = path.read_bytes()
                with self.assertRaisesRegex(adapter.AdapterError, "outbox_binding_"):
                    adapter.requeue_dead_letter(config, entry)
                with self.assertRaisesRegex(adapter.AdapterError, "outbox_binding_"):
                    adapter.enqueue(config, params, allow_quarantined=True)
                self.assertEqual(path.read_bytes(), before)
                self.assertFalse(list((Path(config["state"]) / "outbox").glob("*.json")))

    def test_unicode_query_is_byte_bounded_and_capture_is_character_bounded(self):
        config = self.install(capture_chars=256)
        self.packet(adapter.run_hook(config, self.event(body="记忆🧠" * 3000), self.fake))
        recalled = next(params for method, params in self.fake.calls if method == "recall")
        self.assertLessEqual(len(recalled["query"].encode("utf-8")), 4096)
        candidate = next(iter(self.fake.candidates.values()))
        self.assertLessEqual(len(candidate["body"]), 256)
        self.assertIn("truncated", candidate["body"])

    def test_capture_can_be_disabled_without_disabling_refresh(self):
        config = self.install(capture_user=False, capture_assistant=False)
        self.packet(adapter.run_hook(config, self.event(), self.fake))
        self.assertEqual(adapter.run_hook(config, self.event("Stop"), self.fake), {})
        self.assertFalse(self.fake.candidates)

    def test_hook_from_old_host_snapshot_is_inert_after_uninstall(self):
        config = self.install()
        with patch.object(adapter, "client_for", return_value=self.fake):
            adapter.uninstall(str(self.project))
        self.fake.calls.clear()
        output = adapter.run_hook(config, self.event(), self.fake)
        self.assertIn("inactive", output["hookSpecificOutput"]["additionalContext"])
        self.assertFalse(self.fake.calls)
        self.assertEqual(adapter.run_hook(config, self.event("Stop"), self.fake), {})

    def test_permanent_candidate_error_is_quarantined_and_does_not_block_other_entries(self):
        config = self.install()
        with adapter.locked(Path(config["state"])):
            for event in ("first", "second"):
                adapter.enqueue(config, {
                    "event_id": event, "body": "Valid proposal", "origin": "user_message",
                    "source": "fixture", "idempotency_key": event,
                })
            paths = sorted((Path(config["state"]) / "outbox").glob("*.json"))
            bad_id = adapter.read_json(paths[0])["params"]["event_id"]
            original_call = self.fake.call

            def reject_one(method, params):
                if params["event_id"] == bad_id:
                    raise MemoryError("invalid_request", "invalid fixture payload", False)
                return original_call(method, params)

            self.fake.call = reject_one
            result = adapter.flush_outbox(config, self.fake, deadline=time.monotonic() + 1)
        self.assertEqual(result["quarantined"], 1)
        self.assertEqual(result["delivered"], 1)
        self.assertEqual(result["queued"], 0)
        entries = adapter.dead_letters(config)["entries"]
        self.assertEqual(entries[0]["event_id"], bad_id)
        self.assertFalse(entries[0]["body_included"])
        quarantined = adapter.read_json(adapter.dead_letter_path(config, entries[0]["entry"]))
        self.assertEqual(quarantined["params"]["idempotency_key"], bad_id)
        self.assertFalse(adapter.enqueue(config, quarantined["params"]))
        adapter.requeue_dead_letter(config, entries[0]["entry"])
        self.assertEqual(adapter.dead_letters(config)["total"], 0)
        self.assertEqual(len(list((Path(config["state"]) / "outbox").glob("*.json"))), 1)

    def test_schema_or_retryable_errors_preserve_queue_instead_of_mass_quarantine(self):
        config = self.install()
        with adapter.locked(Path(config["state"])):
            adapter.enqueue(config, {
                "event_id": "retry", "body": "Valid proposal", "origin": "user_message",
                "source": "fixture", "idempotency_key": "retry",
            })
            for code, retryable in (("method_not_found", False), ("schema_mismatch", False), ("busy", True)):
                with self.subTest(code=code):
                    with patch.object(self.fake, "call", side_effect=MemoryError(code, "fixture", retryable)):
                        result = adapter.flush_outbox(config, self.fake, deadline=time.monotonic() + 1)
                    self.assertEqual(result["quarantined"], 0)
                    self.assertEqual(result["queued"], 1)
                    self.assertEqual(result["error"], code)

    def test_project_mismatch_and_symlink_refused(self):
        config = self.install()
        event = dict(self.event(), cwd=str(self.root))
        with self.assertRaisesRegex(adapter.AdapterError, "hook_project_mismatch"):
            adapter.run_hook(config, event, self.fake)
        linked = self.root / "linked"
        linked.symlink_to(self.project, target_is_directory=True)
        with self.assertRaisesRegex(adapter.AdapterError, "symlink_path_refused"):
            adapter.install(str(linked), self.socket)
        self.assertFalse(self.fake.calls)

    def test_default_uninstall_keeps_native_disabled_and_exports_current_state(self):
        self.settings.parent.mkdir()
        original = {"autoMemoryEnabled": True, "custom": {"kept": 1}}
        self.settings.write_text(json.dumps(original))
        self.install()
        with patch.object(adapter, "client_for", return_value=self.fake):
            result = adapter.uninstall(str(self.project))
        restored = json.loads(self.settings.read_text())
        self.assertEqual(restored, {"autoMemoryEnabled": False, "custom": {"kept": 1}})
        self.assertEqual(result["native_mode"], "kept_disabled")
        fallback = adapter.read_json(Path(result["fallback"]))
        self.assertEqual(fallback["snapshot"]["records"][0]["body"], self.fake.body)
        self.assertIn("not a restorable backup", fallback["warning"])
        self.assertEqual(Path(result["fallback"]).stat().st_mode & 0o777, 0o600)

    def test_explicit_reviewed_uninstall_restores_exact_original_bytes(self):
        self.settings.parent.mkdir()
        original = b'{ "autoMemoryEnabled":true,\n "userSetting": [1,2] }\n'
        self.settings.write_bytes(original)
        self.install()
        with patch.object(adapter, "client_for", return_value=self.fake):
            adapter.uninstall(str(self.project), enable_native_after_review=True)
        self.assertEqual(self.settings.read_bytes(), original)

    def test_uninstall_refuses_changed_config_or_unavailable_export(self):
        self.install()
        changed = self.settings.read_bytes() + b" "
        self.settings.write_bytes(changed)
        with patch.object(adapter, "client_for", return_value=self.fake):
            with self.assertRaisesRegex(adapter.AdapterError, "settings_changed_uninstall_refused"):
                adapter.uninstall(str(self.project))
        self.assertEqual(self.settings.read_bytes(), changed)
        self.assertFalse(self.fake.calls)
        self.settings.write_bytes(changed[:-1])
        with patch.object(adapter, "client_for", side_effect=TransportError("offline", False)):
            with self.assertRaises(TransportError):
                adapter.uninstall(str(self.project))
        self.assertIn("hooks", json.loads(self.settings.read_text()))

    def test_uninstall_detects_edits_during_export(self):
        self.install()
        original_call = self.fake.call

        def mutate_during_export(method, params):
            result = original_call(method, params)
            self.settings.write_text('{"userChanged": true}')
            return result

        self.fake.call = mutate_during_export
        with patch.object(adapter, "client_for", return_value=self.fake):
            with self.assertRaisesRegex(adapter.AdapterError, "settings_changed_uninstall_refused"):
                adapter.uninstall(str(self.project))
        self.assertEqual(json.loads(self.settings.read_text()), {"userChanged": True})

    def test_uninstall_recovers_interruptions_before_and_after_settings_write(self):
        for after_write in (False, True):
            with self.subTest(after_write=after_write):
                self.install()
                manifest_path = self.project / ".claude" / "recall-ledger" / "installation.json"
                original_write = adapter.atomic_write
                interrupted = []

                def write_with_fault(path, data):
                    payload = json.loads(data)
                    if not interrupted and (
                        (after_write and path == manifest_path and not payload.get("active", True))
                        or (not after_write and path == self.settings)
                    ):
                        interrupted.append(True)
                        raise OSError("simulated crash")
                    return original_write(path, data)

                with patch.object(adapter, "client_for", return_value=self.fake):
                    with patch.object(adapter, "atomic_write", side_effect=write_with_fault):
                        with self.assertRaises(OSError):
                            adapter.uninstall(str(self.project))
                    self.assertTrue(adapter.read_json(manifest_path)["active"])
                    self.assertIn("uninstall_intent", adapter.read_json(manifest_path))
                    with self.assertRaisesRegex(adapter.AdapterError, "uninstall_in_progress_retry_uninstall"):
                        self.install()
                    exported = len([m for m, _ in self.fake.calls if m == "export"])
                    result = adapter.uninstall(str(self.project))
                self.assertTrue(result["resumed"])
                self.assertEqual(len([m for m, _ in self.fake.calls if m == "export"]), exported)
                self.assertFalse(adapter.read_json(self.settings)["autoMemoryEnabled"])
                self.assertFalse(adapter.read_json(manifest_path)["active"])

    def test_explicit_migration_before_install_keeps_source_unchanged_and_is_idempotent(self):
        source = self.root / "native-memory"
        source.mkdir()
        raw = "# Memory\n用户选择 Rust。\n".encode("utf-8")
        (source / "MEMORY.md").write_bytes(raw)
        (source / "ignore.txt").write_text("Not Markdown")
        with patch.object(adapter, "client_for", return_value=self.fake):
            first = adapter.migrate(str(self.project), str(source), self.socket)
            second = adapter.migrate(str(self.project), str(source), self.socket)
        self.assertFalse(self.settings.exists())
        self.assertEqual(first["admitted"], 0)
        self.assertEqual(second["admitted"], 0)
        self.assertEqual(len(self.fake.candidates), 1)
        proposal = next(iter(self.fake.candidates.values()))
        self.assertEqual(proposal["origin"], "import")
        self.assertEqual(proposal["source"], "claude-code:import")
        self.assertEqual((source / "MEMORY.md").read_bytes(), raw)
        receipts = list((self.project / ".claude" / "recall-ledger" / "imports").glob("*.json"))
        self.assertEqual(len(receipts), 1)
        chunks = adapter.read_json(receipts[0])["chunks"]
        self.assertEqual(chunks, [{
            "event_id": proposal["event_id"], "part": 0,
            "start_char": 0, "end_char": len(raw.decode("utf-8")),
        }])

    def test_migration_refuses_symlink_file(self):
        source = self.root / "native-memory"
        source.mkdir()
        other = self.root / "other.md"
        other.write_text("Not allowed through symlink")
        (source / "linked.md").symlink_to(other)
        with patch.object(adapter, "client_for", return_value=self.fake):
            result = adapter.migrate(str(self.project), str(source), self.socket)
        self.assertEqual(len(result["failures"]), 1)
        self.assertFalse(self.fake.candidates)


@unittest.skipUnless(os.environ.get("RECALL_LEDGER_BIN"), "set RECALL_LEDGER_BIN to test a built Rust daemon")
class RealDaemonAdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rl-h-", dir="/tmp")
        self.root = Path(self.temp.name).resolve()
        self.project = self.root / "project"
        self.project.mkdir()
        self.socket = self.root / "s"
        self.db = self.root / "memory.db"
        self.log = (self.root / "daemon.log").open("wb")
        self.process = subprocess.Popen(
            [os.environ["RECALL_LEDGER_BIN"], "--db", str(self.db), "--namespace", "fixture",
             "serve", "--socket", str(self.socket), "--no-worker"],
            stdout=self.log, stderr=self.log,
        )
        self.client = Client(str(self.socket), timeout=2)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                self.fail("daemon terminated: " + (self.root / "daemon.log").read_text())
            try:
                self.client.call("status")
                break
            except TransportError:
                time.sleep(0.025)
        else:
            self.fail("daemon did not start")
        adapter.install(str(self.project), str(self.socket), timeout=2)
        self.config = adapter.load_config(str(self.project))

    def restart_daemon(self, *, database=None, namespace="fixture"):
        self.process.terminate()
        self.process.wait(timeout=5)
        self.socket.unlink(missing_ok=True)
        self.process = subprocess.Popen(
            [os.environ["RECALL_LEDGER_BIN"], "--db", str(database or self.db),
             "--namespace", namespace, "serve", "--socket", str(self.socket), "--no-worker"],
            stdout=self.log, stderr=self.log,
        )
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                self.fail("replacement daemon terminated: " + (self.root / "daemon.log").read_text())
            try:
                self.client.binding()
                return
            except TransportError:
                time.sleep(0.025)
        self.fail("replacement daemon did not start")

    def tearDown(self):
        if hasattr(self, "process"):
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        if hasattr(self, "log"):
            self.log.close()
        self.temp.cleanup()

    def prompt(self, text):
        return adapter.run_hook(self.config, {
            "session_id": "real-daemon-fixture", "cwd": str(self.project),
            "hook_event_name": "UserPromptSubmit", "prompt": text,
        })

    def test_capture_review_revision_retraction_and_uninstall_against_rust(self):
        body = "Rust memory infrastructure stores canonical records."
        self.prompt(body)
        self.prompt(body)  # Identical host event is deduplicated by daemon.
        listed = self.client.candidate_list(state="pending")
        self.assertEqual(listed["total"], 1)
        candidate = listed["candidates"][0]["candidate_id"]
        self.assertFalse(self.client.recall("Rust", mode="lexical")["items"])
        proposed = self.client.candidate_inspect(candidate)
        self.assertEqual(proposed["body"], body)
        accepted = self.client.candidate_resolve(candidate, "accept", idempotency_key="fixture-approval")
        record_id = accepted["record_id"]
        first = json.loads(self.prompt("Rust")["hookSpecificOutput"]["additionalContext"].split("\n", 1)[1])
        self.assertEqual(first["items"][0]["body"], body)
        manifest = first["manifest"]
        self.assertTrue(self.client.validate_context(manifest)["valid"])
        self.client.revise(record_id, expected_revision=1, body="Rust memory now has revisions.", idempotency_key="fixture-revise")
        self.assertFalse(self.client.validate_context(manifest)["valid"])
        second = json.loads(self.prompt("Rust")["hookSpecificOutput"]["additionalContext"].split("\n", 1)[1])
        self.assertEqual(second["items"][0]["revision"], 2)
        self.client.retract(record_id, expected_revision=2, idempotency_key="fixture-retract")
        third = json.loads(self.prompt("Rust")["hookSpecificOutput"]["additionalContext"].split("\n", 1)[1])
        self.assertEqual(third["items"], [])
        result = adapter.uninstall(str(self.project))
        self.assertEqual(adapter.read_json(Path(result["fallback"]))["snapshot"]["records"], [])
        self.assertFalse(adapter.read_json(self.project / ".claude" / "settings.local.json")["autoMemoryEnabled"])

    def test_cli_entrypoint_accepts_documented_unicode_hook_envelope(self):
        event = {
            "session_id": "cli-fixture", "cwd": str(self.project),
            "hook_event_name": "UserPromptSubmit", "prompt": "记忆基础设施 🧠",
        }
        result = subprocess.run(
            [os.sys.executable, "-m", "recall_ledger.claude_code", "hook", "--project", str(self.project)],
            input=json.dumps(event, ensure_ascii=False).encode("utf-8"),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
        )
        self.assertEqual(result.stderr, b"")
        output = json.loads(result.stdout)
        self.assertEqual(output["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit")
        self.assertIn("untrusted retrieved data", output["hookSpecificOutput"]["additionalContext"])
        candidates = self.client.candidate_list(state="pending")
        self.assertEqual(candidates["total"], 1)
        self.assertEqual(candidates["candidates"][0]["origin"], "user_message")

    def test_migration_skips_whitespace_only_chunks_against_rust(self):
        source = self.root / "native"
        source.mkdir()
        capture_size = self.config["capture_chars"]
        body = "A" * capture_size + " " * capture_size + "Rust memory migration."
        (source / "MEMORY.md").write_text(body)
        result = adapter.migrate(str(self.project), str(source))
        self.assertEqual(result["delivery"]["delivered"], 2)
        self.assertEqual(result["delivery"]["queued"], 0)
        self.assertEqual(result["delivery"]["quarantined"], 0)
        self.assertEqual(self.client.candidate_list(state="pending")["total"], 2)
        receipt = adapter.read_json(next((Path(self.config["state"]) / "imports").glob("*.json")))
        self.assertEqual([chunk["part"] for chunk in receipt["chunks"]], [0, 2])
        self.assertEqual((source / "MEMORY.md").read_text(), body)

    def test_offline_hook_flushes_original_request_once_after_same_database_restart(self):
        old_body = "Rust canonical memory must not be cached during an outage."
        self.client.remember(old_body, idempotency_key="before-outage-source")
        online = self.prompt("Rust")["hookSpecificOutput"]["additionalContext"]
        self.assertIn(old_body, online)
        pending_before = self.client.candidate_list(state="pending")["total"]
        receipt_path = adapter.session_path(self.config, "real-daemon-fixture")
        before_receipt = receipt_path.read_bytes()
        before_binding = dict(self.config["expected_binding"])
        self.process.terminate()
        self.process.wait(timeout=5)

        body = "Offline original proposal 🧠"
        offline = self.prompt(body)["hookSpecificOutput"]["additionalContext"]
        self.assertIn("No cached memory body", offline)
        self.assertNotIn(old_body, offline)
        self.assertEqual(receipt_path.read_bytes(), before_receipt)
        paths = list((Path(self.config["state"]) / "outbox").glob("*.json"))
        self.assertEqual(len(paths), 1)
        request = adapter.read_json(paths[0])
        self.assertEqual(request["expected_binding"], before_binding)
        self.assertEqual(request["params"]["body"], body)
        original_event = request["params"]["event_id"]
        original_key = request["params"]["idempotency_key"]
        self.assertEqual(original_key, original_event)

        self.restart_daemon()
        bound_client = adapter.client_for(self.config)
        self.assertEqual(bound_client.expected_binding, before_binding)
        self.assertEqual(bound_client.binding(), before_binding)
        with patch.object(bound_client, "call", wraps=bound_client.call) as calls:
            with adapter.locked(Path(self.config["state"])):
                first = adapter.flush_outbox(self.config, bound_client, deadline=time.monotonic() + 2)
                second = adapter.flush_outbox(self.config, bound_client, deadline=time.monotonic() + 2)
        self.assertEqual(calls.call_count, 1)
        self.assertEqual(calls.call_args.args, ("candidate_submit", request["params"]))
        self.assertEqual((first["delivered"], first["queued"], first["quarantined"]), (1, 0, 0))
        self.assertIsNone(first["error"])
        self.assertEqual((second["delivered"], second["queued"], second["quarantined"]), (0, 0, 0))
        self.assertIsNone(second["error"])
        self.assertFalse(paths[0].exists())

        committed = bound_client.operation("candidate_submit", original_key)
        self.assertEqual(committed["event_id"], original_event)
        self.assertEqual(committed["state"], "pending")
        inspected = bound_client.candidate_inspect(committed["candidate_id"])
        self.assertEqual(inspected["body"], body)
        listed = bound_client.candidate_list(state="pending")
        self.assertEqual(listed["total"], pending_before + 1)
        self.assertEqual(sum(item["event_id"] == original_event for item in listed["candidates"]), 1)

    def assert_replacement_is_not_used(self, *, namespace, database):
        self.client.remember("Rust old private memory", idempotency_key="old-source")
        self.prompt("Rust")  # Establish the old session's commit barrier.
        adapter.enqueue(self.config, {
            "event_id": "offline-before-replacement", "body": "Old private proposal",
            "origin": "user_message", "source": "fixture",
            "idempotency_key": "offline-before-replacement",
        })
        queued = next((Path(self.config["state"]) / "outbox").glob("*.json"))
        original_queue = queued.read_bytes()
        receipt = adapter.session_path(self.config, "real-daemon-fixture")
        original_receipt = receipt.read_bytes()
        original_settings = (self.project / ".claude" / "settings.local.json").read_bytes()
        self.restart_daemon(namespace=namespace, database=database)
        replacement_binding = self.client.binding()
        self.assertNotEqual(replacement_binding, self.config["expected_binding"])
        self.client.remember("Rust FOREIGN_DATABASE_BODY", idempotency_key="replacement-source")
        self.assertEqual(self.client.candidate_list(state="pending")["total"], 0)

        output = self.prompt("Rust")
        context = output["hookSpecificOutput"]["additionalContext"]
        self.assertIn("binding_mismatch", context)
        self.assertNotIn("FOREIGN_DATABASE_BODY", context)
        self.assertNotIn("Rust old private memory", context)
        self.assertEqual(self.client.candidate_list(state="pending")["total"], 0)
        self.assertEqual(queued.read_bytes(), original_queue)
        self.assertEqual(receipt.read_bytes(), original_receipt)
        with self.assertRaises(MemoryError) as caught:
            adapter.uninstall(str(self.project))
        self.assertEqual(caught.exception.code, "binding_mismatch")
        self.assertEqual((self.project / ".claude" / "settings.local.json").read_bytes(), original_settings)
        with self.assertRaises(MemoryError) as caught:
            adapter.install(str(self.project), str(self.socket), timeout=2)
        self.assertEqual(caught.exception.code, "binding_mismatch")
        self.assertEqual(adapter.load_config(str(self.project)), self.config)

    def test_same_socket_different_namespace_cannot_receive_queue_or_return_memory(self):
        self.assert_replacement_is_not_used(namespace="foreign", database=self.db)

    def test_same_socket_different_history_cannot_receive_queue_or_return_memory(self):
        self.assert_replacement_is_not_used(namespace="fixture", database=self.root / "other.db")


if __name__ == "__main__":
    unittest.main()
