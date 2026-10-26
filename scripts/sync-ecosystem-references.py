"""Synchronize exact-source CI contracts from committed release inputs; defaults to verification."""
import argparse
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
CONSUMERS = ("args", "fidan-editors", "fidan.dev")


def synchronize(root, compiler_sha, write=False):
    sources = json.loads((root / "config/ecosystem-sources.json").read_text())
    release = json.loads((root / "config/release-candidate.json").read_text())
    selected = {**sources, "compiler_sha": compiler_sha}
    for key in ("compiler_sha", "setup_sha", "args_sha", "dal_sha"):
        if not re.fullmatch(r"[a-f0-9]{40}", selected.get(key, "")):
            raise ValueError(f"{key} must be a full immutable commit SHA")
    for name, sha in (("fidan", compiler_sha), ("fidan-actions", sources["setup_sha"]),
                      ("args", sources["args_sha"]), ("dal", sources["dal_sha"])):
        actual = subprocess.check_output(["git", "-C", str(root.parent / name), "rev-parse", sha + "^{commit}"], text=True).strip()
        if actual != sha:
            raise ValueError("Source commit could not be verified")
    changes = {}
    for name in CONSUMERS:
        repository = root.parent / name
        path = repository / "ci/compiler-candidate.json"
        previous = json.loads(path.read_text())
        updated = {**previous, "compiler_sha": compiler_sha, "compiler_version": release["compiler_version"],
                   "setup_sha": sources["setup_sha"]}
        for family in ("args", "dal"):
            if family + "_sha" in previous:
                updated[family + "_sha"] = sources[family + "_sha"]
        if updated != previous:
            changes[path] = json.dumps(updated, indent=2) + "\n"
        # Workflow bootstrap must select the same verifier as its metadata.
        for workflow in (repository / ".github/workflows").glob("*.y*ml"):
            old = workflow.read_text()
            new = old.replace(previous["setup_sha"], sources["setup_sha"])
            if new != old:
                changes[workflow] = new
    for name in ("fidan", "dal"):
        workflow = root.parent / name / ".github/workflows/ci.yaml"
        old = workflow.read_text()
        new, count = re.subn(r"(repository: fidan-lang/fidan-actions\s+ref: )[a-f0-9]{40}",
                            lambda match: match[1] + sources["setup_sha"], old)
        if count != 1:
            raise ValueError("Expected one explicit Setup Action verifier checkout")
        if new != old:
            changes[workflow] = new
    for path, content in changes.items():
        print(path.relative_to(root.parent))
        if write:
            path.write_text(content, encoding="utf-8")
    return changes


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler-sha", required=True, help="Final committed compiler source; never a branch name")
    parser.add_argument("--write", action="store_true", help="Apply changes; otherwise fail if references drift")
    options = parser.parse_args()
    changed = synchronize(ROOT, options.compiler_sha, options.write)
    if changed and not options.write:
        parser.exit(1, "Source contracts differ; inspect and rerun with --write\n")
