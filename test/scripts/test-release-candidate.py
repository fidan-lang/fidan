"""Run: python test/scripts/test-release-candidate.py (offline, no publication)."""
import importlib.util
import json
from pathlib import Path
import tempfile
import sys
sys.dont_write_bytecode = True
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("candidate", ROOT / "scripts/prepare-release-candidate.py")
candidate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidate)


class CandidateTests(unittest.TestCase):
    def test_protocol_and_exact_args_version_are_one_contract(self):
        value = candidate.configuration()
        self.assertEqual(value["llvm_backend_protocol"], 8)
        self.assertEqual(value["args_compiler_requirement"], "=" + value["compiler_version"])

    def test_unsafe_or_existing_destinations_are_never_overwritten(self):
        with self.assertRaises(ValueError):
            candidate.prepare(ROOT.parent / "outside-candidate", ROOT.parent / "args")
        with self.assertRaises(ValueError):
            candidate.prepare(ROOT / "target", ROOT.parent / "args")

    def test_real_sources_stage_versions_and_lock_without_changing_originals(self):
        names = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT.parent / "args/dal.toml"]
        before = [p.read_bytes() for p in names]
        with tempfile.TemporaryDirectory(prefix="release-prep-test-", dir=ROOT / "target") as temp:
            output = candidate.prepare(Path(temp) / "candidate", ROOT.parent / "args")
            value = candidate.configuration()
            lock = tomllib.loads((output / "fidan/Cargo.lock").read_text())
            self.assertEqual(next(p for p in lock["package"] if p["name"] == "fidan-cli" )["version"], value["compiler_version"])
            self.assertEqual(tomllib.loads((output / "fidan/Cargo.toml").read_text())["workspace"]["package"]["version"], value["compiler_version"])
            self.assertEqual(tomllib.loads((output / "fidan/crates/fidan-llvm-helper/Cargo.toml").read_text())["package"]["version"], value["llvm_helper_version"])
            self.assertEqual(tomllib.loads((output / "args/dal.toml").read_text())["package"]["fidan"], value["args_compiler_requirement"])
            self.assertEqual(json.loads((output / "release-preparation.json").read_text())["status"], "PROPOSED_NOT_RELEASED")
            self.assertEqual((output / "fidan/NOTICE").read_bytes(), (ROOT / "NOTICE").read_bytes())
        self.assertEqual(before, [p.read_bytes() for p in names])


if __name__ == "__main__":
    unittest.main()
