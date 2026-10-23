"""Real stdio JSON-RPC regression for the actual sibling args documents.
Set FIDAN_BIN to the isolated compiler to test. No settings or files are changed.
"""
import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
BINARY = Path(os.environ.get("FIDAN_BIN", ROOT / "target/debug" / ("fidan.exe" if os.name == "nt" else "fidan")))


class Rpc:
    def __init__(self, cli_args=()):
        self.process = subprocess.Popen([str(BINARY), *cli_args, "lsp"], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.stderr = bytearray()
        self.stderr_reader = threading.Thread(target=self.read_stderr, daemon=True)
        self.stderr_reader.start()
        self.messages = queue.Queue()
        self.diagnostics = {}
        self.next_id = 0
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read_stderr(self):
        while chunk := self.process.stderr.read(4096):
            self.stderr.extend(chunk)

    def read(self):
        try:
            while True:
                headers = {}
                while True:
                    line = self.process.stdout.readline()
                    if not line:
                        return
                    if line == b"\r\n":
                        break
                    key, value = line.decode("ascii").split(":", 1)
                    headers[key.lower()] = value.strip()
                length = int(headers["content-length"])
                payload = self.process.stdout.read(length)
                self.messages.put(json.loads(payload))
        except Exception as failure:
            self.messages.put(failure)

    def send(self, method, params=None, request=False):
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        if request:
            self.next_id += 1
            message["id"] = self.next_id
        payload = json.dumps(message).encode("utf-8")
        self.process.stdin.write(f"Content-Length: {len(payload)}\r\n\r\n".encode("ascii") + payload)
        self.process.stdin.flush()
        return message.get("id")

    def wait(self, predicate):
        deadline = time.monotonic() + 30
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError("LSP response timed out")
            message = self.messages.get(timeout=remaining)
            if isinstance(message, Exception):
                raise message
            if message.get("method") == "textDocument/publishDiagnostics":
                self.diagnostics[message["params"]["uri"]] = message["params"]
            if predicate(message):
                return message

    def initialize(self):
        identifier = self.send("initialize", {"processId": os.getpid(),
            "rootUri": ROOT.parent.as_uri(), "capabilities": {}}, request=True)
        response = self.wait(lambda value: value.get("id") == identifier)
        assert "result" in response, response
        self.send("initialized", {})

    def open(self, path, text, version=1):
        uri = path.as_uri()
        self.send("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "fidan", "version": version, "text": text}})
        return self.wait_diagnostics(uri, version)

    def change(self, path, text, version):
        uri = path.as_uri()
        self.send("textDocument/didChange", {"textDocument": {"uri": uri, "version": version},
                   "contentChanges": [{"text": text}]})
        return self.wait_diagnostics(uri, version)

    def wait_diagnostics(self, uri, version):
        self.wait(lambda value: value.get("method") == "textDocument/publishDiagnostics"
                  and value["params"]["uri"] == uri and value["params"].get("version") == version)
        return self.diagnostics[uri]["diagnostics"]

    def close(self):
        if self.process.poll() is None:
            identifier = self.send("shutdown", request=True)
            self.wait(lambda value: value.get("id") == identifier)
            self.send("exit")
            self.process.stdin.close()  # The client closes transport after exit.
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.stderr_reader.join(timeout=5)
        for pipe in (self.process.stdin, self.process.stdout, self.process.stderr):
            pipe.close()
        self.reader.join(timeout=5)


class ArgsLspTests(unittest.TestCase):
    def test_verbose_json_rpc_transport_and_errors(self):
        for flags in [(), ("-v",), ("-vv",), ("-vvv",)]:
            with self.subTest(flags=flags):
                rpc = Rpc(flags)
                try:
                    rpc.initialize()
                    path = ROOT / "target/verbosity-contract/editor.fdn"
                    diagnostics = rpc.open(path, "var value = unknown_verbosity_symbol\n")
                    self.assertTrue(any(item.get("severity") == 1 for item in diagnostics))
                    self.assertFalse(any(item.get("severity") == 1 for item in rpc.change(path, "print(42)\n", 2)))
                    identifier = rpc.send("unsupported/verbosityProbe", {}, request=True)
                    response = rpc.wait(lambda value: value.get("id") == identifier)
                    self.assertEqual(response["error"]["code"], -32601)
                finally:
                    rpc.close()
                self.assertEqual(rpc.process.returncode, 0)
                logs = rpc.stderr.decode("utf-8")
                self.assertNotIn("\x1b", logs)
                if flags:
                    self.assertIn("LSP initialize received", logs)
                    self.assertIn("LSP shutdown received", logs)
                else:
                    self.assertNotIn("INFO fidan", logs)

    def test_actual_args_documents_and_negative_controls(self):
        rpc = Rpc()
        try:
            rpc.initialize()
            for relative in ("src/core.fdn", "src/app.fdn", "src/init.fdn", "examples/greet.fdn"):
                path = ROOT.parent / "args" / relative
                text = path.read_text(encoding="utf-8")
                diagnostics = rpc.open(path, text)
                self.assertFalse([item for item in diagnostics if item.get("severity", 1) <= 2],
                                 f"{relative}: {diagnostics}")
                changed = rpc.change(path, text + "\n", 2)
                self.assertFalse([item for item in changed if item.get("severity", 1) <= 2], changed)
            path = ROOT.parent / "args/examples/greet.fdn"
            text = path.read_text(encoding="utf-8")
            invalid = rpc.change(path, text + "\n@missing_decorator_control\naction invalid_control {}\nprint(greet.missing_member_control)\n", 3)
            self.assertTrue(any(item.get("code") == "W2004" for item in invalid), invalid)
            self.assertTrue(any(item.get("code") == "E0204" for item in invalid), invalid)
            corrected = rpc.change(path, text, 4)
            self.assertFalse([item for item in corrected if item.get("severity", 1) <= 2], corrected)
        finally:
            rpc.close()

    def test_compiler_attribute_contracts_and_deprecation_messages(self):
        rpc = Rpc()
        try:
            rpc.initialize()
            path = ROOT / "test/examples/decorator_semantics_regression.fdn"
            source = '@deprecated("Use replacement")\naction old {}\nold()\n'
            diagnostics = rpc.open(path, source)
            self.assertFalse([item for item in diagnostics if item.get("severity", 1) == 1], diagnostics)
            self.assertTrue(any(item.get("code") == "W2005" and "Use replacement" in item["message"] for item in diagnostics), diagnostics)
            invalid = rpc.change(path, '@precompile(42)\naction invalid {}\n', 2)
            self.assertTrue(any(item.get("code") == "E0305" for item in invalid), invalid)
        finally:
            rpc.close()

    def test_short_circuit_does_not_hide_rhs_diagnostics_on_change(self):
        rpc = Rpc()
        try:
            rpc.initialize()
            path = ROOT / "test/examples/short_circuit_regression.fdn"
            text = path.read_text(encoding="utf-8")
            original = rpc.open(path, text)
            self.assertFalse([item for item in original if item.get("severity", 1) == 1], original)
            invalid = rpc.change(path, text + "\nvar invalid_rhs = false and 42\nvar undefined_rhs = true or missing_logical_rhs\n", 2)
            for code in ("E0203", "E0101"):
                self.assertTrue(any(item.get("code") == code and item.get("severity") == 1 for item in invalid), invalid)
            corrected = rpc.change(path, text, 3)
            self.assertFalse([item for item in corrected if item.get("severity", 1) == 1], corrected)
        finally:
            rpc.close()


if __name__ == "__main__":
    print(json.dumps({"binary": str(BINARY), "sha256": hashlib.sha256(BINARY.read_bytes()).hexdigest(),
                      "version": subprocess.check_output([str(BINARY), "--version"], text=True).strip()}), flush=True)
    unittest.main()
