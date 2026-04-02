#!/usr/bin/env sh

set -eu

VERSION="latest"
MANIFEST_URL="${FIDAN_DIST_MANIFEST:-https://releases.fidan.dev/manifest.json}"
INSTALL_ROOT=""
SKIP_PATH_UPDATE=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      VERSION="$2"
      shift 2
      ;;
    --manifest-url)
      MANIFEST_URL="$2"
      shift 2
      ;;
    --install-root)
      INSTALL_ROOT="$2"
      shift 2
      ;;
    --skip-path-update)
      SKIP_PATH_UPDATE=1
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "required command not found: $1" >&2
    exit 1
  }
}

PYTHON_BIN=""
if command -v python3 >/dev/null 2>&1; then
  PYTHON_BIN="python3"
elif command -v python >/dev/null 2>&1; then
  PYTHON_BIN="python"
else
  echo "python3 or python is required for the bootstrap script" >&2
  exit 1
fi

need_cmd tar
if command -v curl >/dev/null 2>&1; then
  DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
  DOWNLOADER="wget"
else
  echo "curl or wget is required for the bootstrap script" >&2
  exit 1
fi

resolve_install_root() {
  if [ -n "$INSTALL_ROOT" ]; then
    printf '%s\n' "$INSTALL_ROOT"
    return
  fi

  case "$(uname -s)" in
    Darwin)
      printf '%s\n' "$HOME/Applications/Fidan"
      ;;
    *)
      if [ -n "${XDG_DATA_HOME:-}" ]; then
        printf '%s\n' "$XDG_DATA_HOME/fidan/installs"
      else
        printf '%s\n' "$HOME/.local/share/fidan/installs"
      fi
      ;;
  esac
}

host_triple() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$arch" in
    x86_64|amd64) arch_part="x86_64" ;;
    arm64|aarch64) arch_part="aarch64" ;;
    *)
      echo "unsupported architecture: $arch" >&2
      exit 1
      ;;
  esac

  case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux) os_part="unknown-linux-gnu" ;;
    *)
      echo "unsupported operating system: $os" >&2
      exit 1
      ;;
  esac

  printf '%s-%s\n' "$arch_part" "$os_part"
}

download_to() {
  url="$1"
  dest="$2"
  if [ "$DOWNLOADER" = "curl" ]; then
    curl -fsSL "$url" -o "$dest"
  else
    wget -qO "$dest" "$url"
  fi
}

sha256_of() {
  path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
    return
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
    return
  fi
  "$PYTHON_BIN" - "$path" <<'PY'
import hashlib, pathlib, sys
path = pathlib.Path(sys.argv[1])
print(hashlib.sha256(path.read_bytes()).hexdigest())
PY
}

read_release_field() {
  manifest_path="$1"
  requested_version="$2"
  current_host="$3"
  "$PYTHON_BIN" - "$manifest_path" "$requested_version" "$current_host" <<'PY'
import json
import pathlib
import sys

manifest_path = pathlib.Path(sys.argv[1])
requested = sys.argv[2]
host = sys.argv[3]
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
if manifest.get("schema_version", 0) == 0:
    raise SystemExit("distribution manifest has invalid schema_version 0")

releases = [r for r in manifest.get("fidan_versions", []) if r.get("host_triple") == host]
if not releases:
    raise SystemExit(f"no Fidan releases are available for host '{host}'")

def parse_version_tuple(version):
    parts = []
    for item in version.split("."):
        digits = ""
        for ch in item:
            if ch.isdigit():
                digits += ch
            else:
                break
        parts.append(int(digits or "0"))
    return tuple(parts)

def is_prerelease(version):
    return "-" in version or "+" in version

def sort_key(release):
    version = release.get("version", "")
    return (
        parse_version_tuple(version),
        0 if is_prerelease(version) else 1,
        version,
    )

releases.sort(key=sort_key, reverse=True)
if requested != "latest":
    matches = [r for r in releases if r.get("version") == requested]
    if not matches:
        raise SystemExit(f"Fidan version '{requested}' is not available for '{host}'")
    release = matches[0]
else:
    release = releases[0]

binary_relpath = release.get("binary_relpath")
if not binary_relpath:
    binary_relpath = "fidan.exe" if sys.platform.startswith("win") else "fidan"

print(release["version"])
print(release["url"])
print(release["sha256"])
print(binary_relpath)
PY
}

update_metadata() {
  metadata_dir="$1"
  version="$2"
  make_active="$3"
  mkdir -p "$metadata_dir"
  now="$(date +%s)"
  installs_path="$metadata_dir/installs.json"
  active_path="$metadata_dir/active-version.json"
  "$PYTHON_BIN" - "$installs_path" "$active_path" "$version" "$now" "$make_active" <<'PY'
import json
import pathlib
import sys

installs_path = pathlib.Path(sys.argv[1])
active_path = pathlib.Path(sys.argv[2])
version = sys.argv[3]
now = int(sys.argv[4])
make_active = sys.argv[5] == "1"

if installs_path.exists():
    installs = json.loads(installs_path.read_text(encoding="utf-8"))
else:
    installs = {"schema_version": 1, "installs": [], "updated_at_secs": now}

if not any(entry.get("version") == version for entry in installs.get("installs", [])):
    installs.setdefault("installs", []).append(
        {"version": version, "installed_at_secs": now}
    )
installs["schema_version"] = 1
installs["updated_at_secs"] = now
installs_path.write_text(json.dumps(installs, indent=2) + "\n", encoding="utf-8")

if make_active or not active_path.exists():
    active = {
        "schema_version": 1,
        "active_version": version,
        "updated_at_secs": now,
    }
    active_path.write_text(json.dumps(active, indent=2) + "\n", encoding="utf-8")
PY
}

ensure_path() {
  current_dir="$1"
  if [ "$SKIP_PATH_UPDATE" -eq 1 ]; then
    return
  fi

  profile="$HOME/.profile"
  case "${SHELL:-}" in
    */zsh) profile="$HOME/.zprofile" ;;
    */bash) profile="$HOME/.bash_profile" ;;
  esac

  mkdir -p "$(dirname "$profile")"
  touch "$profile"
  path_line="export PATH=\"$current_dir:\$PATH\""
  if ! grep -F "$current_dir" "$profile" >/dev/null 2>&1; then
    printf '\n%s\n' "$path_line" >>"$profile"
    echo "Added $current_dir to PATH in $profile. Open a new shell to pick it up."
  fi
}

INSTALL_ROOT_RESOLVED="$(resolve_install_root)"
HOST_TRIPLE="$(host_triple)"
TMPDIR_FIDAN="$(mktemp -d "${TMPDIR:-/tmp}/fidan-bootstrap.XXXXXX")"
MANIFEST_PATH="$TMPDIR_FIDAN/manifest.json"
ARCHIVE_PATH="$TMPDIR_FIDAN/fidan.tar.gz"
EXTRACT_DIR="$TMPDIR_FIDAN/extract"
mkdir -p "$EXTRACT_DIR"

cleanup() {
  rm -rf "$TMPDIR_FIDAN"
}
trap cleanup EXIT INT TERM

echo "Fetching manifest from $MANIFEST_URL"
download_to "$MANIFEST_URL" "$MANIFEST_PATH"

RELEASE_INFO="$(read_release_field "$MANIFEST_PATH" "$VERSION" "$HOST_TRIPLE")"
RELEASE_VERSION="$(printf '%s\n' "$RELEASE_INFO" | sed -n '1p')"
ARCHIVE_URL="$(printf '%s\n' "$RELEASE_INFO" | sed -n '2p')"
EXPECTED_SHA="$(printf '%s\n' "$RELEASE_INFO" | sed -n '3p')"
BINARY_RELPATH="$(printf '%s\n' "$RELEASE_INFO" | sed -n '4p')"

VERSIONS_DIR="$INSTALL_ROOT_RESOLVED/versions"
METADATA_DIR="$INSTALL_ROOT_RESOLVED/metadata"
FINAL_DIR="$VERSIONS_DIR/$RELEASE_VERSION"
CURRENT_LINK="$INSTALL_ROOT_RESOLVED/current"

if [ -e "$FINAL_DIR" ]; then
  echo "Fidan version '$RELEASE_VERSION' is already installed at '$FINAL_DIR'" >&2
  exit 1
fi

mkdir -p "$VERSIONS_DIR" "$METADATA_DIR"
FIRST_INSTALL=1
if [ -d "$VERSIONS_DIR" ] && [ "$(find "$VERSIONS_DIR" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" -gt 0 ]; then
  FIRST_INSTALL=0
fi

echo "Downloading Fidan $RELEASE_VERSION for $HOST_TRIPLE"
download_to "$ARCHIVE_URL" "$ARCHIVE_PATH"
ACTUAL_SHA="$(sha256_of "$ARCHIVE_PATH")"
if [ "$ACTUAL_SHA" != "$(printf '%s' "$EXPECTED_SHA" | tr '[:upper:]' '[:lower:]')" ]; then
  echo "SHA-256 mismatch for '$ARCHIVE_URL' (expected $EXPECTED_SHA, got $ACTUAL_SHA)" >&2
  exit 1
fi

tar -xzf "$ARCHIVE_PATH" -C "$EXTRACT_DIR"

CANDIDATE_ROOT="$EXTRACT_DIR"
if [ ! -e "$CANDIDATE_ROOT/$BINARY_RELPATH" ]; then
  set -- "$EXTRACT_DIR"/*
  if [ "$#" -ne 1 ] || [ ! -d "$1" ]; then
    echo "Downloaded archive does not contain '$BINARY_RELPATH' at the root or inside a single top-level directory" >&2
    exit 1
  fi
  CANDIDATE_ROOT="$1"
  if [ ! -e "$CANDIDATE_ROOT/$BINARY_RELPATH" ]; then
    echo "Downloaded archive does not contain the expected file '$BINARY_RELPATH'" >&2
    exit 1
  fi
fi

mv "$CANDIDATE_ROOT" "$FINAL_DIR"
update_metadata "$METADATA_DIR" "$RELEASE_VERSION" "$FIRST_INSTALL"
if [ "$FIRST_INSTALL" -eq 1 ]; then
  ln -sfn "$FINAL_DIR" "$CURRENT_LINK"
  ensure_path "$CURRENT_LINK"
  echo "Installed Fidan $RELEASE_VERSION and made it active"
else
  echo "Installed Fidan $RELEASE_VERSION"
  echo "Run 'fidan self use $RELEASE_VERSION' to activate it"
fi
echo "Install root: $INSTALL_ROOT_RESOLVED"
