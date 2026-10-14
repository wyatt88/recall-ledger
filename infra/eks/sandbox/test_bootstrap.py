"""Offline fixtures only: never install a runtime or contact a service."""
import ast
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("bootstrap.sh")


def call(*args, **extra):
    env = dict(os.environ, RUNSC_RELEASE="20261005", RUNSC_ARCH="x86_64")
    env.update(extra)
    return subprocess.run(["bash", str(SCRIPT), *args], env=env, capture_output=True, text=True, timeout=10)


def archive(path, members):
    with tarfile.open(path, "w:bz2") as output:
        for name, content, kind in members:
            entry = tarfile.TarInfo(name)
            entry.mode = 0o755
            entry.type = kind
            entry.size = len(content) if kind == tarfile.REGTYPE else 0
            if kind == tarfile.SYMTYPE:
                entry.linkname = "/etc/passwd"
            output.addfile(entry, io.BytesIO(content) if kind == tarfile.REGTYPE else None)
    return hashlib.sha512(path.read_bytes()).hexdigest()


BASE = [("runsc", b"not executed", tarfile.REGTYPE),
        ("containerd-shim-runsc-v1", b"not executed", tarfile.REGTYPE),
        ("gvisor-bin/sentry", b"not executed", tarfile.REGTYPE)]


class BootstrapTests(unittest.TestCase):
    def test_bash_syntax(self):
        result = subprocess.run(["bash", "-n", str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_render_adds_one_runtime_without_replacing_eks_defaults(self):
        result = call("--print-nodeconfig")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('io.containerd.cri.v1.runtime".containerd.runtimes.runsc-offline', result.stdout)
        self.assertIn('runtime_path = "/opt/recall-ledger/gvisor/20261005-x86_64/containerd-shim-runsc-v1"', result.stdout)
        self.assertIn('ConfigPath = "/etc/containerd/recall-runsc-offline.toml"', result.stdout)
        for forbidden in ("default_runtime_name", "sandbox_image", "imports =", "registry]", "cni]"):
            self.assertNotIn(forbidden, result.stdout)
        config = call("--print-runsc-config").stdout
        for setting in ('platform = "systrap"', 'network = "none"', 'systemd-cgroup = "true"',
                        'allow-flag-override = "false"'):
            self.assertIn(setting, config)

    def test_render_only_allows_exact_shared_volume_annotation_keys(self):
        result = call("--print-nodeconfig")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.count("pod_annotations = "), 1)
        rendered = result.stdout.split("pod_annotations = ", 1)[1].split("]", 1)[0] + "]"
        expected = [
            f"dev.gvisor.spec.mount.{volume}.{setting}"
            for volume in ("tmp",)
            for setting in ("share", "type", "options")
        ]
        self.assertEqual(ast.literal_eval(rendered), expected)
        self.assertNotIn("dev.gvisor.*", result.stdout)
        self.assertNotIn("dev.gvisor.spec.mount.data.", result.stdout)
        self.assertNotIn("container_annotations", result.stdout)

    def test_mutable_release_and_missing_first_boot_guard_fail(self):
        self.assertNotEqual(call("--print-nodeconfig", RUNSC_RELEASE="latest").returncode, 0)
        self.assertNotEqual(call("--print-nodeconfig", RUNSC_RELEASE="../20261005").returncode, 0)
        result = call("--install", RUNSC_SHA512="a" * 128, ALLOW_NEW_NODE_BOOTSTRAP="0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("first-boot guard", result.stderr)

    def test_complete_multifile_archive_verifies_without_executing(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "gvisor.tar.bz2"
            digest = archive(path, [("./", b"", tarfile.DIRTYPE)] + BASE)
            result = call("--check-archive", str(path), RUNSC_SHA512=digest)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("no runtime executed", result.stdout)
            self.assertEqual(list(Path(temp).iterdir()), [path])

    def test_checksum_failure_rejects_before_extracting(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "gvisor.tar.bz2"
            archive(path, BASE)
            result = call("--check-archive", str(path), RUNSC_SHA512="0" * 128)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("SHA512 mismatch", result.stderr)

    def test_missing_sidecars_traversal_links_and_duplicates_fail(self):
        fixtures = [
            BASE[:2],
            BASE + [("../outside", b"x", tarfile.REGTYPE)],
            BASE + [("gvisor-bin/link", b"", tarfile.SYMTYPE)],
            BASE + [("runsc", b"x", tarfile.REGTYPE)],
            [("runsc", b"", tarfile.DIRTYPE)] + BASE[1:],
        ]
        with tempfile.TemporaryDirectory() as temp:
            for index, members in enumerate(fixtures):
                with self.subTest(index=index):
                    path = Path(temp) / ("%d.tar.bz2" % index)
                    digest = archive(path, members)
                    result = call("--check-archive", str(path), RUNSC_SHA512=digest)
                    self.assertNotEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
