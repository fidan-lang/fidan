"""Prepare isolated release sources; never edit a release, tag or live checkout."""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def configuration(root=ROOT):
    candidate = json.loads((root / "config/release-candidate.json").read_text())
    if candidate["status"] != "PREPARED_NOT_RELEASED":
        raise ValueError("Only explicitly prepared, unpublished releases may be staged")
    for key in ("compiler_version", "llvm_helper_version", "ai_analysis_helper_version"):
        if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", candidate[key]):
            raise ValueError("Release preparation requires stable SemVer versions")
    if candidate["llvm_helper_version"] != candidate["compiler_version"]:
        raise ValueError("Compiler and protocol-8 helper must use the same stable release version")
    source = (root / "crates/fidan-driver/src/llvm_helper.rs").read_text()
    protocol = int(re.search(r"LLVM_BACKEND_PROTOCOL_VERSION: u32 = (\d+)", source)[1])
    if candidate["llvm_backend_protocol"] != protocol:
        raise ValueError("Candidate protocol differs from compiler source")
    ai_source = (root / "crates/fidan-driver/src/ai_analysis.rs").read_text()
    ai_protocol = int(re.search(r"AI_ANALYSIS_HELPER_PROTOCOL_VERSION: u32 = (\d+)", ai_source)[1])
    if candidate["ai_analysis_helper_protocol"] != ai_protocol:
        raise ValueError("AI helper protocol differs from compiler source")
    if candidate["args_compiler_requirement"] != "=" + candidate["compiler_version"]:
        raise ValueError("args must select the exact rehearsed candidate")
    return candidate


def copy_source(source, destination):
    files = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=source).decode().split("\0")
    for name in set(files) - {""}:
        file = source / name
        if file.is_symlink():
            raise ValueError("Source export refuses symbolic links")
        if not file.is_file():
            continue
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(file, target)


def prepare(output, args_root, root=ROOT):
    output = output.resolve()
    if not output.is_relative_to((root / "target").resolve()):
        raise ValueError("Candidate output must remain below this checkout target/")
    if output.exists():
        raise ValueError("Candidate destination already exists; never overwrite")
    candidate = configuration(root)
    args_root = args_root.resolve()
    args_package = tomllib.loads((args_root / "dal.toml").read_text())["package"]
    if args_package["name"] != "args" or args_package["version"] != candidate["args_version"]:
        raise ValueError("Expected the actual args source repository at the selected package version")
    output.mkdir(parents=True)
    compiler = output / "fidan"
    copy_source(root, compiler)
    copy_source(args_root, output / "args")
    manifest = compiler / "Cargo.toml"
    old = tomllib.loads(manifest.read_text())["workspace"]["package"]["version"]
    manifest.write_text(manifest.read_text().replace('version = "' + old + '"', 'version = "' + candidate["compiler_version"] + '"', 1))
    helper = compiler / "crates/fidan-llvm-helper/Cargo.toml"
    old_helper = tomllib.loads(helper.read_text())["package"]["version"]
    helper.write_text(helper.read_text().replace('version = "' + old_helper + '"', 'version = "' + candidate["llvm_helper_version"] + '"', 1))
    ai_helper = compiler / "crates/fidan-ai-analysis-helper/Cargo.toml"
    old_ai = tomllib.loads(ai_helper.read_text())["package"]["version"]
    ai_helper.write_text(ai_helper.read_text().replace('version = "' + old_ai + '"', 'version = "' + candidate["ai_analysis_helper_version"] + '"', 1))
    package = output / "args/dal.toml"
    sentinel = 'fidan = "=0.0.0-args-unreleased"'
    if sentinel not in package.read_text():
        raise ValueError("Only the explicit unpublished args source may be prepared")
    package.write_text(package.read_text().replace(sentinel, 'fidan = "' + candidate["args_compiler_requirement"] + '"'))
    subprocess.run(["cargo", "update", "--workspace", "--offline"], cwd=compiler, check=True, stdout=subprocess.DEVNULL)
    (output / "release-preparation.json").write_text(json.dumps(candidate, indent=2) + "\n")
    return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--args-root", type=Path, default=ROOT.parent / "args")
    options = parser.parse_args()
    print(prepare(options.output, options.args_root))
