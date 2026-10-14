#!/usr/bin/env python3
"""Host commands are simulated; these tests never contact AWS or a cluster."""

import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest


spec = importlib.util.spec_from_file_location("egress_guard", Path(__file__).with_name("egress_guard.py"))
guard_module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = guard_module
spec.loader.exec_module(guard_module)
Config = guard_module.Config
Guard = guard_module.Guard
GuardError = guard_module.GuardError


class FakeNode:
    def __init__(self):
        self.calls = []
        self.boot = "test-boot"
        self.live = True
        self.route_dev = "enitest123"
        self.default_dev = "ens5"
        self.default_nexthops = []
        self.gateway_local = True
        self.link = {
            "ifname": "enitest123", "ifindex": 7, "link_index": 3,
            "address": "02:00:00:00:00:01", "linkinfo": {"info_kind": "veth"},
        }
        self.chains = {}
        for tool in ("iptables", "ip6tables"):
            for table, chain in (("mangle", "PREROUTING"), ("filter", "FORWARD"), ("filter", "OUTPUT")):
                self.chains[tool, table, chain] = [("-m", "comment", "--comment", "existing", "-j", "ACCEPT")]
        self.fail = None
        self.listing_error = None

    def __call__(self, args):
        self.calls.append(args)
        if self.fail and self.fail(args):
            return subprocess.CompletedProcess(args, 4, "", "Permission denied")
        if args[0] == "ip":
            query = args[2:]
            if query[:2] == ["route", "get"]:
                result = [{"dev": self.route_dev, "dst": query[-1]}]
            elif query[:3] in (["-4", "route", "show"], ["-6", "route", "show"]):
                result = [{"dev": self.default_dev, "nexthops": self.default_nexthops}]
            elif query[:3] == ["-4", "address", "show"]:
                result = [{"ifname": "ens5", "addr_info": [{"local": "10.0.0.10"}]}] if self.gateway_local else []
            elif query[:3] == ["-d", "link", "show"]:
                result = [self.link] if self.live else []
            else:
                raise AssertionError(f"unexpected ip call: {args}")
            return subprocess.CompletedProcess(args, 0, json.dumps(result), "")
        tool, wait, seconds, flag, table, action, *remaining = args
        assert (wait, seconds, flag) == ("-w", "5", "-t")
        if action == "-S":
            if remaining:
                return subprocess.CompletedProcess(
                    args, 1, "",
                    "iptables v1.8.8 (nf_tables): chain is incompatible, use 'nft' tool.",
                )
            if self.listing_error:
                return subprocess.CompletedProcess(args, 1, "", self.listing_error)
            entries = [(key[2], rules) for key, rules in self.chains.items() if key[:2] == (tool, table)]
            lines = [
                shlex.join(["-P", chain, "ACCEPT"] if chain in {"PREROUTING", "FORWARD", "OUTPUT"} else ["-N", chain])
                for chain, _ in entries
            ]
            lines += [
                shlex.join(["-A", chain, *rule])
                for chain, rules in entries for rule in rules
            ]
            return subprocess.CompletedProcess(args, 0, "\n".join(lines), "")
        chain, *rule = remaining
        key = (tool, table, chain)
        if action == "-N":
            assert key not in self.chains
            self.chains[key] = []
        elif action == "-A":
            self.chains[key].append(tuple(rule))
        elif action == "-I":
            position, *rule = rule
            self.chains[key].insert(int(position) - 1, tuple(rule))
        elif action == "-D":
            self.chains[key].remove(tuple(rule))
        elif action == "-X":
            assert not self.chains[key]
            del self.chains[key]
        else:
            raise AssertionError(f"unexpected firewall mutation: {args}")
        return subprocess.CompletedProcess(args, 0, "", "")


class GuardTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.node = FakeNode()
        self.config = Config("10.0.0.25", "10.0.0.10", "enitest123")
        self.guard = Guard(
            self.config, Path(self.temporary.name) / "state", self.node,
            lambda: self.node.boot, lambda: 1234,
        )

    def mutations(self):
        return [call for call in self.node.calls if call[0] != "ip" and call[5] != "-S"]

    def test_install_exact_allowlist_and_all_ipv6_denied(self):
        result = self.guard.install()
        self.assertEqual(result["status"], "verified")
        self.assertEqual(len(result["rules"]), 6)
        for binding in self.guard.bindings:
            if binding.tool == "ip6tables":
                self.assertEqual(binding.rules, (("-j", "DROP"),))
        egress = self.guard.bindings[0]
        self.assertIn("10.0.0.10/32", egress.rules[0])
        self.assertIn("18080", egress.rules[0])
        self.assertNotIn("53", str([b.rules for b in self.guard.bindings]))
        reply = self.guard.bindings[2]
        self.assertEqual(reply.table, "filter")
        self.assertIn("REPLY", reply.rules[0])
        self.assertEqual(self.guard.state_path.stat().st_mode & 0o777, 0o600)

    def test_all_deny_hooks_installed_before_first_permission(self):
        self.guard.install()
        writes = self.mutations()
        first_permission = next(i for i, call in enumerate(writes) if "RETURN" in call)
        hooks = [call for call in writes[:first_permission] if call[5] == "-I"]
        self.assertEqual(len(hooks), 6)

    def test_install_is_idempotent_and_verify_is_read_only(self):
        self.guard.install()
        count = len(self.mutations())
        self.guard.install()
        self.guard.verify()
        self.assertEqual(len(self.mutations()), count)

    def test_default_interface_is_rejected_before_mutation(self):
        self.node.default_dev = self.config.veth
        with self.assertRaisesRegex(GuardError, "default-route"):
            self.guard.install()
        self.assertFalse(self.mutations())

    def test_multipath_default_interface_is_rejected(self):
        self.node.default_nexthops = [{"dev": self.config.veth}]
        with self.assertRaisesRegex(GuardError, "default-route"):
            self.guard.install()
        self.assertFalse(self.mutations())

    def test_non_veth_wrong_route_and_nonlocal_gateway_are_rejected(self):
        for field, value in (("route_dev", "ens5"), ("gateway_local", False)):
            with self.subTest(field=field):
                original = getattr(self.node, field)
                setattr(self.node, field, value)
                with self.assertRaises(GuardError):
                    self.guard.install()
                setattr(self.node, field, original)
        self.node.link["linkinfo"]["info_kind"] = "dummy"
        with self.assertRaisesRegex(GuardError, "not a veth"):
            self.guard.install()
        self.assertFalse(self.mutations())

    def test_interface_reuse_and_reboot_cannot_adopt_old_state(self):
        self.guard.install()
        self.node.link["ifindex"] += 1
        count = len(self.mutations())
        with self.assertRaisesRegex(GuardError, "identity changed"):
            self.guard.install()
        self.node.link["ifindex"] -= 1
        self.node.boot = "next-boot"
        with self.assertRaisesRegex(GuardError, "identity changed"):
            self.guard.verify()
        self.assertEqual(len(self.mutations()), count)

    def test_rule_tamper_or_earlier_jump_fails_verification(self):
        self.guard.install()
        binding = self.guard.bindings[0]
        key = (binding.tool, binding.table, binding.chain)
        self.node.chains[key].insert(0, ("-j", "RETURN"))
        with self.assertRaisesRegex(GuardError, "chain changed"):
            self.guard.verify()
        self.node.chains[key].pop(0)
        hook = (binding.tool, binding.table, binding.hook)
        self.node.chains[hook].insert(0, ("-j", "ACCEPT"))
        with self.assertRaisesRegex(GuardError, "not first"):
            self.guard.verify()

    def test_missing_ipv6_support_is_not_qualified(self):
        self.node.fail = lambda args: args[0] == "ip6tables"
        with self.assertRaisesRegex(GuardError, "read failed"):
            self.guard.install()
        self.assertFalse(self.mutations())

    def test_nft_absent_chain_uses_complete_successful_listing(self):
        self.guard.install()
        reads = [call for call in self.node.calls if call[0] != "ip" and call[5] == "-S"]
        self.assertTrue(reads)
        self.assertTrue(all(len(call) == 6 for call in reads))

    def test_incompatible_table_is_never_treated_as_missing(self):
        self.node.listing_error = "table is incompatible, use 'nft' tool"
        with self.assertRaisesRegex(GuardError, "incompatible"):
            self.guard.install()
        self.assertFalse(self.mutations())
        self.assertFalse(self.guard.state_path.exists())

    def test_failed_install_preserves_deny_and_requires_cleanup(self):
        self.node.fail = lambda args: "-I" in args and "RETURN" in args
        with self.assertRaises(GuardError):
            self.guard.install()
        self.assertEqual(json.loads(self.guard.state_path.read_text())["phase"], "installing")
        for binding in self.guard.bindings:
            self.assertEqual(self.node.chains[binding.tool, binding.table, binding.chain], [("-j", "DROP")])
        self.node.fail = None
        with self.assertRaisesRegex(GuardError, "incomplete"):
            self.guard.install()
        self.node.live = False
        self.assertEqual(self.guard.remove()["status"], "removed")

    def test_live_remove_is_refused_then_only_owned_rules_are_removed(self):
        original = {key: list(value) for key, value in self.node.chains.items()}
        self.guard.install()
        with self.assertRaisesRegex(GuardError, "veth to disappear"):
            self.guard.remove()
        self.node.live = False
        self.guard.remove()
        self.assertEqual(self.node.chains, original)
        self.assertFalse(any("-F" in call for call in self.node.calls))
        self.assertEqual(self.guard.remove()["status"], "already_removed")

    def test_unowned_chain_collision_is_not_adopted(self):
        binding = self.guard.bindings[0]
        self.node.chains[binding.tool, binding.table, binding.chain] = []
        with self.assertRaisesRegex(GuardError, "collision"):
            self.guard.install()
        self.assertFalse(self.mutations())

    def test_remove_preserves_unknown_rules(self):
        self.guard.install()
        binding = self.guard.bindings[-1]
        key = (binding.tool, binding.table, binding.chain)
        self.node.chains[key].append(("-m", "comment", "--comment", "foreign", "-j", "DROP"))
        self.node.live = False
        with self.assertRaisesRegex(GuardError, "unknown rules"):
            self.guard.remove()
        self.assertEqual(self.node.chains[key], [("-m", "comment", "--comment", "foreign", "-j", "DROP")])
        self.assertTrue(self.guard.state_path.exists())

    def test_invalid_input_has_no_shell_or_address_ambiguity(self):
        for pod, gateway, veth in (
            ("10.0.0.25/32", "10.0.0.10", "enitest123"),
            ("::1", "10.0.0.10", "enitest123"),
            ("10.0.0.25", "169.254.169.254", "enitest123"),
            ("10.0.0.25", "10.0.0.10", "$(id)"),
            ("10.0.0.25", "10.0.0.10", "lo"),
        ):
            with self.subTest(pod=pod, gateway=gateway, veth=veth):
                with self.assertRaises(GuardError):
                    Config(pod, gateway, veth)

    def test_state_symlink_is_rejected(self):
        self.guard.state_dir.mkdir(mode=0o700)
        target = Path(self.temporary.name) / "untrusted"
        target.write_text("{}")
        self.guard.state_path.symlink_to(target)
        with self.assertRaisesRegex(GuardError, "not a symlink"):
            self.guard.install()
        self.assertFalse(self.mutations())

    def test_malformed_state_is_not_adopted(self):
        self.guard.state_dir.mkdir(mode=0o700)
        self.guard.state_path.write_text("[]")
        self.guard.state_path.chmod(0o600)
        with self.assertRaisesRegex(GuardError, "state does not match"):
            self.guard.install()
        self.assertFalse(self.mutations())


if __name__ == "__main__":
    unittest.main()
