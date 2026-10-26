"""Publish a validated bundle without replacing immutable releases or losing manifest updates."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import urlsplit, unquote


def read(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def identities(manifest, family):
    fields = ("version", "host_triple") if family == "fidan_versions" else ("kind", "toolchain_version", "host_triple")
    values = manifest[family]
    result = {tuple(value[field] for field in fields): value for value in values}
    if len(result) != len(values):
        raise ValueError("Duplicate release identity in manifest")
    return result


def validate_preservation(old, new):
    if old["schema_version"] != 1 or new["schema_version"] != 1:
        raise ValueError("Unsupported distribution manifest")
    for family in ("fidan_versions", "toolchains"):
        existing, incoming = identities(old, family), identities(new, family)
        if any(incoming.get(key) != value for key, value in existing.items()):
            raise ValueError("Refusing to remove or replace an existing immutable release")


def aws(operation, *arguments):
    result = subprocess.run(["aws", "s3api", operation, "--bucket", os.environ["R2_BUCKET"],
                             "--endpoint-url", os.environ["R2_ENDPOINT"], "--output", "json", *arguments],
                            capture_output=True, text=True, timeout=1800)
    if result.returncode:
        # AWS failures may include credential-bearing configuration; do not echo it.
        raise RuntimeError(f"R2 {operation} failed; publication stopped without bypassing conditions")
    return json.loads(result.stdout or "{}")


def publish(bundle):
    bundle = bundle.resolve()
    old, new = read(bundle / "existing-manifest.json"), read(bundle / "manifest.json")
    validate_preservation(old, new)
    payload = bundle / "payload"
    files = sorted(path for path in payload.rglob("*") if path.is_file())
    if not files or any(path.is_symlink() or path.stat().st_size > 5 * 1024**3 for path in files):
        raise ValueError("Payload must contain regular files within the single-object upload limit")
    existing_paths = {unquote(urlsplit(value["url"]).path).lstrip("/")
                      for family in ("fidan_versions", "toolchains") for value in old[family]}
    new_prefixes = set()
    for family in ("fidan_versions", "toolchains"):
        previous = identities(old, family)
        for key, entry in identities(new, family).items():
            if key in previous:
                continue
            url = urlsplit(entry["url"])
            relative = unquote(url.path).lstrip("/")
            archive = payload / relative
            if (url.scheme != "https" or url.netloc != "releases.fidan.dev" or url.query or
                    url.fragment or not archive.resolve().is_relative_to(payload.resolve()) or
                    not archive.is_file() or digest(archive) != entry["sha256"]):
                raise ValueError("New archive path, origin or checksum differs from its manifest")
            new_prefixes.add(archive.parent)
    for path in files:
        if path.parent not in new_prefixes or path.relative_to(payload).as_posix() in existing_paths:
            raise ValueError("Payload contains an unrelated or already published artifact")
    compiler_versions = {entry["version"] for key, entry in identities(new, "fidan_versions").items()
                         if key not in identities(old, "fidan_versions")}
    if compiler_versions:
        if len(compiler_versions) != 1:
            raise ValueError("Publish only one compiler version per bundle")
        version = compiler_versions.pop()
        changelog = (Path(__file__).resolve().parents[1] / "CHANGELOG.md").read_text(encoding="utf-8")
        heading = "## [" + version + "]"
        if heading not in changelog:
            raise ValueError("Missing versioned release notes")
        notes = heading + changelog.split(heading, 1)[1].split("\n## ", 1)[0]
        notes += "\nInstallation and optional toolchains: https://github.com/fidan-lang/fidan#installation\n"
        (bundle / "release-notes.md").write_text(notes, encoding="utf-8")
    # Read and compare immediately before upload; the final conditional write also
    # rejects publishers racing after this check. Never retry a stale manifest.
    with tempfile.TemporaryDirectory(prefix="publish-", dir=bundle) as directory:
        current = Path(directory) / "manifest.json"
        metadata = aws("get-object", "--key", "manifest.json", str(current))
        if read(current) != old or not metadata.get("ETag"):
            raise ValueError("Remote manifest changed since bundling; rebuild the bundle before publishing")
    for path in files:
        key = path.relative_to(payload).as_posix()
        aws("put-object", "--key", key, "--body", str(path), "--if-none-match", "*",
            "--cache-control", "no-cache")
        print(f"Published immutable artifact: {key}")
    aws("put-object", "--key", "manifest.json", "--body", str(bundle / "manifest.json"),
        "--if-match", metadata["ETag"], "--content-type", "application/json", "--cache-control", "no-cache")
    print("Published manifest; all previous release entries preserved")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    publish(parser.parse_args().bundle)
