"""Offline publication invariants; no credentials or network required."""
import sys
sys.dont_write_bytecode = True
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("publisher", ROOT / "scripts/publish-distribution.py")
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)


class PublicationTests(unittest.TestCase):
    def test_old_versions_cannot_be_removed_or_replaced(self):
        old = {"schema_version": 1, "fidan_versions": [{"version": "1.0.16", "host_triple": "x", "sha256": "old"}], "toolchains": []}
        for new in ({**old, "fidan_versions": []}, {**old, "fidan_versions": [{**old["fidan_versions"][0], "sha256": "changed"}]}):
            with self.assertRaises(ValueError):
                publisher.validate_preservation(old, new)
        publisher.validate_preservation(old, old)

    def test_immutable_payload_and_manifest_use_atomic_conditions(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory)
            (bundle / "payload/fidan/1.1.0/host").mkdir(parents=True)
            payload = bundle / "payload/fidan/1.1.0/host/archive.tar.gz"
            payload.write_bytes(b"compiler")
            old = {"schema_version": 1, "fidan_versions": [], "toolchains": []}
            new = {**old, "fidan_versions": [{"version": "1.1.0", "host_triple": "host", "url": "https://releases.fidan.dev/fidan/1.1.0/host/archive.tar.gz", "sha256": publisher.digest(payload)}]}
            for name, value in (("existing-manifest.json", old), ("manifest.json", new)):
                (bundle / name).write_text(json.dumps(value))
            calls = []
            def aws(operation, *args):
                calls.append((operation, args))
                if operation == "get-object":
                    Path(args[-1]).write_text(json.dumps(old))
                    return {"ETag": '"previous"'}
                return {}
            with patch.object(publisher, "aws", side_effect=aws):
                publisher.publish(bundle)
            puts = [args for operation, args in calls if operation == "put-object"]
            self.assertEqual(len(puts), 2)
            self.assertIn("--if-none-match", puts[0])
            self.assertIn("*", puts[0])
            self.assertIn("--if-match", puts[1])
            self.assertIn('"previous"', puts[1])
            # A failed conditional artifact write must never advance the manifest.
            calls.clear()
            def denied(operation, *args):
                if operation == "put-object":
                    calls.append((operation, args))
                    raise RuntimeError("precondition failed")
                return aws(operation, *args)
            with patch.object(publisher, "aws", side_effect=denied), self.assertRaises(RuntimeError):
                publisher.publish(bundle)
            self.assertEqual(len([op for op, args in calls if op == "put-object"]), 1)
            # A stale bundle must never write any artifact or manifest.
            calls.clear()
            def changed(operation, *args):
                calls.append(operation)
                Path(args[-1]).write_text(json.dumps({**old, "toolchains": [{"new": "release"}]}))
                return {"ETag": '"changed"'}
            with patch.object(publisher, "aws", side_effect=changed), self.assertRaises(ValueError):
                publisher.publish(bundle)
            self.assertEqual(calls, ["get-object"])
            payload.write_bytes(b"corrupt")
            with patch.object(publisher, "aws") as cloud, self.assertRaises(ValueError):
                publisher.publish(bundle)
            cloud.assert_not_called()

    def test_duplicate_identities_are_rejected(self):
        value = {"schema_version": 1, "fidan_versions": [], "toolchains": [{"kind": "llvm", "toolchain_version": "1.1.0", "host_triple": "x"}] * 2}
        with self.assertRaises(ValueError):
            publisher.validate_preservation(value, value)


if __name__ == "__main__":
    unittest.main()
