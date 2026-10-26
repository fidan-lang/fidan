"""Exercise the actual sweep command construction, including macOS Bash 3.2."""
import os
import json
import tempfile
from pathlib import Path
import shutil
import subprocess
import unittest


class SweepInvocationTests(unittest.TestCase):
    def test_empty_arguments_benchmark_and_stdin_preserve_boundaries(self):
        bash = os.environ.get("BASH")
        if not bash and os.name == "nt":
            # PATH can select the WSL stub on hosted Windows; use Git's Bash.
            git = shutil.which("git")
            git_bash = Path(git).resolve().parent.parent / "bin/bash.exe" if git else None
            if git_bash and git_bash.is_file():
                bash = str(git_bash)
        bash = bash or shutil.which("bash")
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
            ("performance_suite.fdn", "5", ["/path with spaces/program", "two words", "two words", "two words"]),
            ("replay_demo.fdn", "10", ["--stdin-lines", r"6\n3", "/path with spaces/program"]),
        ):
            with self.subTest(name=name):
                result = subprocess.run([bash, "--noprofile", "--norc", "-c", setup + dispatch, "sweep-regression", name],
                                        capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", errors="replace"))
                self.assertEqual(result.stderr, b"")
                values = result.stdout.decode("utf-8").split("\0")
                self.assertEqual(values, [timeout, "stdout path", "stderr path", *arguments, ""])


    def test_powershell_sweep_bounds_every_benchmark_workload(self):
        pwsh = shutil.which("pwsh")
        self.assertIsNotNone(pwsh, "PowerShell is required for its sweep regression")
        repo = Path(__file__).resolve().parents[2]
        source = Path(__file__).with_name("test_examples_aot.ps1").read_text(encoding="utf-8")
        start = source.index("        $timeoutMs = $DefaultTimeoutSeconds * 1000")
        stop = source.index("        $exitCode = Invoke-ProgramWithTimeout", start)
        script = """$DefaultTimeoutSeconds = 10
$BenchmarkProbeSeconds = 5
$BenchmarkWorkItems = 12345
$baseName = $args[0]
""" + source[start:stop] + """
@{timeout=$timeoutMs; arguments=@($runArguments); stdin=$stdinLines} | ConvertTo-Json -Compress
"""
        (repo / "target").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="sweep-invocation-", dir=repo / "target") as temporary:
            probe = Path(temporary) / "dispatch.ps1"
            probe.write_text(script, encoding="utf-8")
            for name, timeout, args, stdin in (
                ("async_demo.fdn", 10000, [], None),
                ("parallel_benchmark.fdn", 5000, ["12345"], None),
                ("performance_suite.fdn", 5000, ["12345", "12345", "12345"], None),
                ("replay_demo.fdn", 10000, [], ["6", "3"]),
            ):
                with self.subTest(name=name):
                    result = subprocess.run([pwsh, "-NoProfile", "-File", str(probe), name],
                                            capture_output=True, text=True, encoding="utf-8", timeout=20)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(json.loads(result.stdout), {"timeout":timeout, "arguments":args, "stdin":stdin})


if __name__ == "__main__":
    unittest.main()
