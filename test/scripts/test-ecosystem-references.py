"""Offline exact-ref synchronization checks using the real sibling source contracts."""
import sys
sys.dont_write_bytecode = True
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("sync_refs", ROOT / "scripts/sync-ecosystem-references.py")
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)


class ReferenceTests(unittest.TestCase):
    def test_mutable_or_incomplete_references_are_rejected(self):
        for sha in ("main", "latest", "a" * 39, "A" * 40, "../main"):
            with self.assertRaises(ValueError):
                sync.synchronize(ROOT, sha)

    def test_sync_is_narrow_and_idempotent(self):
        (ROOT / "target").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=ROOT / "target") as directory:
            parent = Path(directory)
            for name in ("ecosystem-sources.json", "release-candidate.json"):
                destination = parent / "fidan/config" / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / "config" / name, destination)
            previous = {"compiler_repository": "fidan-lang/fidan", "compiler_sha": "b" * 40,
                        "compiler_version": "1.0.16", "setup_sha": "c" * 40}
            for name in sync.CONSUMERS:
                manifest = parent / name / "ci/compiler-candidate.json"
                manifest.parent.mkdir(parents=True, exist_ok=True)
                manifest.write_text(json.dumps(previous))
            for name in ("fidan", "dal", *sync.CONSUMERS):
                workflow = parent / name / ".github/workflows/ci.yaml"
                workflow.parent.mkdir(parents=True, exist_ok=True)
                workflow.write_text("repository: fidan-lang/fidan-actions\nref: " + "c" * 40 + "\n")
            def revision(arguments, **kwargs):
                return arguments[-1].removesuffix("^{commit}") + "\n"
            with patch.object(sync.subprocess, "check_output", side_effect=revision):
                self.assertTrue(sync.synchronize(parent / "fidan", "a" * 40))
                sync.synchronize(parent / "fidan", "a" * 40, write=True)
                self.assertFalse(sync.synchronize(parent / "fidan", "a" * 40))
            for name in sync.CONSUMERS:
                value = json.loads((parent / name / "ci/compiler-candidate.json").read_text())
                self.assertEqual(value["compiler_sha"], "a" * 40)
                self.assertEqual(value["compiler_version"], "1.1.0")


if __name__ == "__main__":
    unittest.main()
