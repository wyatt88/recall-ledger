import copy
from pathlib import Path
import tempfile
import unittest

from recall_ledger.environment import capture_environment, check_environment

NOW = "2026-11-09T10:00:00Z"


class EnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        # macOS /var is a symlink. Hosts must hand us the resolved controlled root.
        self.root = Path(self.temp.name).resolve()
        (self.root / "lock").write_bytes(b"version=1")

    def tearDown(self):
        self.temp.cleanup()

    def capture(self, files=None, at=NOW):
        return capture_environment(roots={"project": self.root},
                                   files=files or [{"root": "project", "path": "lock"}],
                                   tool_versions={"runner": "v1"}, observed_at=at)

    def test_same_environment_matches_and_versions_and_files_change(self):
        original = self.capture()
        self.assertEqual(check_environment(original, self.capture(), now=NOW)["status"], "applicable")
        (self.root / "lock").write_bytes(b"version=2")
        self.assertEqual(check_environment(original, self.capture(), now=NOW)["status"], "mismatch")
        changed = copy.deepcopy(original)
        changed["tools"]["runner"] = "v2"
        self.assertEqual(check_environment(original, changed, now=NOW)["status"], "mismatch")
        self.assertNotIn(str(self.root), str(original))

    def test_missing_file_and_missing_version_are_unknown(self):
        original = self.capture()
        (self.root / "lock").unlink()
        current = self.capture()
        current["tools"] = {}
        result = check_environment(original, current, now=NOW)
        self.assertEqual(result["status"], "unknown")
        self.assertFalse(result["permissions_granted"])

    def test_explicit_timezones_stale_future_and_invalid_clock(self):
        original = self.capture(at="2026-11-09T18:00:00+08:00")
        self.assertEqual(original["observed_at"], NOW)
        self.assertEqual(check_environment(original, original, now="2026-11-11T10:00:00Z")["status"], "unknown")
        self.assertEqual(check_environment(original, original, now="2026-11-08T10:00:00Z")["status"], "unknown")
        with self.assertRaises(ValueError):
            self.capture(at="2026-11-09T10:00:00")
        with self.assertRaises(ValueError):
            check_environment(original, original, now=NOW, max_age_seconds=-1)

    def test_root_escape_and_unknown_alias_are_rejected(self):
        for path in ("../outside", "/etc/passwd", "nested/../../outside", "./lock"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                self.capture([{"root": "project", "path": path}])
        with self.assertRaises(ValueError):
            self.capture([{"root": "other", "path": "lock"}])

    def test_symlink_file_and_symlink_directory_cannot_escape(self):
        (self.root / "link").symlink_to(self.root / "lock")
        (self.root / "dirlink").symlink_to(self.root, target_is_directory=True)
        for path in ("link", "dirlink/lock"):
            result = self.capture([{"root": "project", "path": path}])
            self.assertEqual(result["files"][0]["status"], "unknown")

    def test_empty_requirements_and_duplicate_observations_do_not_match(self):
        current = self.capture()
        empty = dict(current, files=[], tools={})
        self.assertEqual(check_environment(empty, current, now=NOW)["status"], "unknown")
        duplicate = copy.deepcopy(current)
        duplicate["files"].append(copy.deepcopy(duplicate["files"][0]))
        with self.assertRaises(ValueError):
            check_environment(current, duplicate, now=NOW)
