"""Exercise the actual sweep command construction, including macOS Bash 3.2."""
import os
from pathlib import Path
import shutil
import subprocess
import unittest


class SweepInvocationTests(unittest.TestCase):
    def test_empty_arguments_benchmark_and_stdin_preserve_boundaries(self):
        bash = os.environ.get("BASH") or shutil.which("bash")
        self.assertIsNotNone(bash, "Bash is required for the Unix sweep regression")
        source = Path(__file__).with_name("test_examples_aot.sh").read_text(encoding="utf-8")
        start = source.index('    timeout_secs="$DEFAULT_TIMEOUT_SECONDS"')
        stop = source.index('    exit_code=$?', start)
        dispatch = source[start:stop]
        setup = r'''set -euo pipefail
DEFAULT_TIMEOUT_SECONDS=10
BENCHMARK_PROBE_SECONDS=5
FIDAN_BENCHMARK_WORK_ITEMS='two words'
bin='/path with spaces/program'
stdout='stdout path'
stderr='stderr path'
base_name="$1"
run_with_timeout() { printf '%s\0' "$@"; }
'''
        for name, timeout, arguments in (
            ("async_demo.fdn", "10", ["/path with spaces/program"]),
            ("parallel_benchmark.fdn", "5", ["/path with spaces/program", "two words"]),
            ("replay_demo.fdn", "10", ["--stdin-lines", r"6\n3", "/path with spaces/program"]),
        ):
            with self.subTest(name=name):
                result = subprocess.run([bash, "--noprofile", "--norc", "-c", setup + dispatch, "sweep-regression", name],
                                        capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", errors="replace"))
                self.assertEqual(result.stderr, b"")
                values = result.stdout.decode("utf-8").split("\0")
                self.assertEqual(values, [timeout, "stdout path", "stderr path", *arguments, ""])


if __name__ == "__main__":
    unittest.main()
