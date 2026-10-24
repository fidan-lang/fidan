"""Exercise real workflow run bodies with hostile env data, no credentials/tools.

PyYAML 6.0.3 and PowerShell 7 required. No build/sign/publish step can succeed:
only invalid versions/identifiers are supplied, and the child environment is
explicitly populated from non-credential OS variables plus synthetic inputs.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import yaml

REPO = Path(__file__).resolve().parents[2]

class ExecutableInputTests(unittest.TestCase):
    def test_real_release_run_bodies_treat_inputs_as_data(self):
        pwsh = shutil.which("pwsh")
        self.assertIsNotNone(pwsh, "PowerShell 7 required")
        cases = (
            ("release-distribution.yaml", "Package release artifact", "WF_INPUTS_VERSION", "SemVer", {"WF_GITHUB_EVENT_NAME": "workflow_dispatch", "WF_GITHUB_REF_NAME": "main"}),
            ("release-distribution.yaml", "Package release artifact", "WF_GITHUB_REF_NAME", "SemVer", {"WF_GITHUB_EVENT_NAME": "push"}),
            ("llvm-toolchain-build.yaml", "Resolve LLVM source metadata", "WF_INPUTS_LLVM_TOOL_VERSION", "SemVer", {}),
            ("ai-analysis-toolchain-build.yaml", "Resolve helper version", "WF_INPUTS_TOOL_VERSION", "SemVer", {}),
            ("toolchain-bundle-publish.yaml", "Validate artifact identifiers before download", "ARTIFACT_PATTERN", "Unsafe release artifact", {"BUNDLE_ARTIFACT_NAME": "valid-bundle"}),
        )
        with tempfile.TemporaryDirectory(prefix="workflow-inputs-", dir=REPO / "target") as tmp:
            scratch = Path(tmp)
            marker = scratch / "INJECTED"
            payloads = ('1.0.0"; New-Item -ItemType File -Path \"' + str(marker) + '\"; #',
                        '$(New-Item -ItemType File -Path \"' + str(marker) + '\")',
                        "1.0.0\nFORGED_OUTPUT=1", "../../outside")
            for workflow, step_name, variable, diagnostic, values in cases:
                document = yaml.safe_load((REPO / ".github/workflows" / workflow).read_text(encoding="utf-8"))
                step = next(step for job in document["jobs"].values() for step in job.get("steps", []) if step.get("name") == step_name)
                script = scratch / "step.ps1"
                script.write_text("$ErrorActionPreference = 'Stop'\n" + step["run"], encoding="utf-8")
                for payload in payloads:
                    with self.subTest(workflow=workflow, variable=variable, payload=payload):
                        env = {name: os.environ[name] for name in ("SystemRoot", "WINDIR", "PATH") if name in os.environ}
                        env.update(values)
                        env[variable] = ("v" if variable == "WF_GITHUB_REF_NAME" else "") + payload
                        env["GITHUB_OUTPUT"] = str(scratch / "output")
                        env["TEMP"] = str(scratch)
                        env["TMP"] = str(scratch)
                        result = subprocess.run([pwsh, "-NoProfile", "-NonInteractive", "-File", str(script)], cwd=REPO, env=env,
                                                stdin=subprocess.DEVNULL, capture_output=True, text=True, encoding="utf-8", timeout=20)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn(diagnostic, result.stderr)
                        self.assertFalse(marker.exists(), "Input was executed as shell source")
                        self.assertFalse((scratch / "output").exists(), "Invalid data reached workflow outputs")

if __name__ == "__main__":
    unittest.main()
