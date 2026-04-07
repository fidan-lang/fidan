#!/usr/bin/env sh

set -eu

VERSION="latest"
MANIFEST_URL="${FIDAN_DIST_MANIFEST:-https://releases.fidan.dev/manifest.json}"
INSTALL_ROOT=""
SKIP_PATH_UPDATE=0
ALLOW_EXISTING_INSTALL=0
BANNER_URL="https://raw.githubusercontent.com/fidan-lang/fidan/main/assets/github/banner.txt"
DOWNLOADER=""

choose_downloader() {
  if command -v curl >/dev/null 2>&1; then
    DOWNLOADER="curl"
    return
  fi
  if command -v wget >/dev/null 2>&1; then
    DOWNLOADER="wget"
    return
  fi
  DOWNLOADER=""
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

download_text() {
  url="$1"
  if [ "$DOWNLOADER" = "curl" ]; then
    curl -fsSL "$url"
  else
    wget -qO- "$url"
  fi
}

show_banner() {
  printf '\n'

  script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
  local_banner_path="$script_dir/../assets/github/banner.txt"
  if [ -f "$local_banner_path" ]; then
    cat "$local_banner_path"
    printf '\n'
    return
  fi

  choose_downloader
  if [ -n "$DOWNLOADER" ]; then
    if banner_text="$(download_text "$BANNER_URL" 2>/dev/null)" && [ -n "$banner_text" ]; then
      printf '%s\n\n' "$banner_text"
      return
    fi
  fi

  printf 'FIDAN\n\n'
}

show_usage() {
  cat <<'EOF'
Fidan bootstrap installer

Options:
  --version <version>          Install a specific released version (default: latest)
  --manifest-url <url>         Override the distribution manifest URL
  --install-root <path>        Override the self-managed install root
  --skip-path-update           Do not modify the shell profile PATH entry
  --allow-existing-install     Permit bootstrapping into an existing Fidan install root
  --help                       Show this help text

Bootstrap is intended for first install. If Fidan is already installed,
prefer 'fidan self install' and 'fidan self use'.
EOF
}

RED='\033[31m'
NC='\033[0m' # No Color

fail() {
  message="$1"
  printf "\n${RED}[X] Installation failed:${NC}\n" >&2
  printf "${RED}%s${NC}\n" "$message" >&2
  exit 1
}

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
    --allow-existing-install)
      ALLOW_EXISTING_INSTALL=1
      shift
      ;;
    --help)
      show_banner
      show_usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

show_banner

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    fail "required command not found: $1"
  }
}

need_cmd tar
choose_downloader
if [ -z "$DOWNLOADER" ]; then
  fail "curl or wget is required for the bootstrap script"
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

has_existing_install() {
  root="$1"
  [ -e "$root/current" ] && return 0
  [ -e "$root/metadata/installs.json" ] && return 0
  [ -e "$root/metadata/active-version.json" ] && return 0
  if [ -d "$root/versions" ] && find "$root/versions" -mindepth 1 -maxdepth 1 -type d | grep -q .; then
    return 0
  fi
  return 1
}

host_triple() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$arch" in
    x86_64|amd64) arch_part="x86_64" ;;
    arm64|aarch64) arch_part="aarch64" ;;
    *)
      fail "unsupported architecture: $arch"
      ;;
  esac

  case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux) os_part="unknown-linux-gnu" ;;
    *)
      fail "unsupported operating system: $os"
      ;;
  esac

  printf '%s-%s\n' "$arch_part" "$os_part"
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
  if command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$path" | awk '{print $NF}'
    return
  fi
  fail "sha256sum, shasum, or openssl is required for the bootstrap script"
}

json_string_field() {
  object="$1"
  field="$2"
  printf '%s\n' "$object" | sed -n "s/.*\"$field\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p"
}

version_sort_key() {
  version="$1"
  prerelease_rank=1
  case "$version" in
    *-*|*+*) prerelease_rank=0 ;;
  esac

  base_version="${version%%[-+]*}"
  old_ifs="$IFS"
  IFS='.'
  set -- $base_version
  IFS="$old_ifs"

  printf '%08d.%08d.%08d.%08d.%d.%s\n' "${1:-0}" "${2:-0}" "${3:-0}" "${4:-0}" "$prerelease_rank" "$version"
}

read_release_field() {
  manifest_path="$1"
  requested_version="$2"
  current_host="$3"

  manifest_json="$(tr -d '\r\n' < "$manifest_path")"
  schema_version="$(printf '%s\n' "$manifest_json" | sed -n 's/.*"schema_version"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p')"
  if [ -z "$schema_version" ] || [ "$schema_version" = "0" ]; then
    fail "distribution manifest has invalid schema_version 0"
  fi

  release_block="$(printf '%s\n' "$manifest_json" | sed -n 's/.*"fidan_versions"[[:space:]]*:[[:space:]]*\[\(.*\)\][[:space:]]*,[[:space:]]*"toolchains".*/\1/p')"
  if [ -z "$release_block" ]; then
    fail "distribution manifest does not contain any fidan_versions"
  fi

  normalized_objects="$(printf '%s\n' "$release_block" | sed 's/}[[:space:]]*,[[:space:]]*{/}\
{/g')"

  old_ifs="$IFS"
  IFS='
'
  set -f

  host_match_count=0
  selected_version=""
  selected_url=""
  selected_sha=""
  selected_binary_relpath=""
  selected_key=""

  for object in $normalized_objects; do
    host_value="$(json_string_field "$object" "host_triple")"
    [ "$host_value" = "$current_host" ] || continue
    host_match_count=$((host_match_count + 1))

    version_value="$(json_string_field "$object" "version")"
    url_value="$(json_string_field "$object" "url")"
    sha_value="$(json_string_field "$object" "sha256")"
    binary_value="$(json_string_field "$object" "binary_relpath")"
    [ -n "$binary_value" ] || binary_value="fidan"

    if [ "$requested_version" != "latest" ]; then
      if [ "$version_value" = "$requested_version" ]; then
        selected_version="$version_value"
        selected_url="$url_value"
        selected_sha="$sha_value"
        selected_binary_relpath="$binary_value"
        break
      fi
      continue
    fi

    candidate_key="$(version_sort_key "$version_value")"
    if [ -z "$selected_key" ] || [ "$candidate_key" \> "$selected_key" ]; then
      selected_key="$candidate_key"
      selected_version="$version_value"
      selected_url="$url_value"
      selected_sha="$sha_value"
      selected_binary_relpath="$binary_value"
    fi
  done

  set +f
  IFS="$old_ifs"

  if [ "$host_match_count" -eq 0 ]; then
    fail "no Fidan releases are available for host '$current_host'"
  fi

  if [ -z "$selected_version" ]; then
    fail "Fidan version '$requested_version' is not available for '$current_host'"
  fi

  printf '%s\n%s\n%s\n%s\n' "$selected_version" "$selected_url" "$selected_sha" "$selected_binary_relpath"
}

path_mtime_secs() {
  path="$1"
  if stat -c %Y "$path" >/dev/null 2>&1; then
    stat -c %Y "$path"
    return
  fi
  if stat -f %m "$path" >/dev/null 2>&1; then
    stat -f %m "$path"
    return
  fi
  date +%s
}

write_installs_metadata() {
  installs_path="$1"
  versions_dir="$2"
  now="$3"
  temp_path="$installs_path.tmp"

  {
    printf '{\n'
    printf '  "schema_version": 1,\n'
    printf '  "installs": [\n'

    first_entry=1
    if [ -d "$versions_dir" ]; then
      for version_dir in "$versions_dir"/*; do
        [ -d "$version_dir" ] || continue
        version_name="${version_dir##*/}"
        installed_at="$(path_mtime_secs "$version_dir")"
        if [ "$first_entry" -eq 0 ]; then
          printf ',\n'
        fi
        first_entry=0
        printf '    {\n'
        printf '      "version": "%s",\n' "$version_name"
        printf '      "installed_at_secs": %s\n' "$installed_at"
        printf '    }'
      done
    fi

    printf '\n  ],\n'
    printf '  "updated_at_secs": %s\n' "$now"
    printf '}\n'
  } > "$temp_path"

  mv "$temp_path" "$installs_path"
}

write_active_metadata() {
  active_path="$1"
  version="$2"
  now="$3"
  temp_path="$active_path.tmp"

  {
    printf '{\n'
    printf '  "schema_version": 1,\n'
    printf '  "active_version": "%s",\n' "$version"
    printf '  "updated_at_secs": %s\n' "$now"
    printf '}\n'
  } > "$temp_path"

  mv "$temp_path" "$active_path"
}

update_metadata() {
  metadata_dir="$1"
  versions_dir="$2"
  version="$3"
  make_active="$4"
  now="$(date +%s)"
  installs_path="$metadata_dir/installs.json"
  active_path="$metadata_dir/active-version.json"

  mkdir -p "$metadata_dir"
  write_installs_metadata "$installs_path" "$versions_dir" "$now"
  if [ "$make_active" = "1" ] || [ ! -e "$active_path" ]; then
    write_active_metadata "$active_path" "$version" "$now"
  fi
}

ensure_path() {
  current_dir="$1"
  if [ "$SKIP_PATH_UPDATE" -eq 1 ]; then
    return
  fi

  os_name="$(uname -s)"
  path_line="case \":\$PATH:\" in *\":$current_dir:\"*) ;; *) export PATH=\"$current_dir:\$PATH\" ;; esac"
  profile_targets=""

  case "${SHELL:-}" in
    */zsh)
      profile_targets="$HOME/.zshrc"
      if [ "$os_name" = "Darwin" ] || [ -f "$HOME/.zprofile" ]; then
        profile_targets="$profile_targets
$HOME/.zprofile"
      fi
      ;;
    */bash)
      profile_targets="$HOME/.bashrc
$HOME/.profile"
      if [ "$os_name" = "Darwin" ] || [ -f "$HOME/.bash_profile" ]; then
        profile_targets="$profile_targets
$HOME/.bash_profile"
      fi
      ;;
    *)
      profile_targets="$HOME/.profile"
      ;;
  esac

  added_files=""
  old_ifs="$IFS"
  IFS='
'
  for profile in $profile_targets; do
    mkdir -p "$(dirname "$profile")"
    touch "$profile"
    if ! grep -F "$current_dir" "$profile" >/dev/null 2>&1; then
      printf '\n%s\n' "$path_line" >>"$profile"
      if [ -z "$added_files" ]; then
        added_files="$profile"
      else
        added_files="$added_files, $profile"
      fi
    fi
  done
  IFS="$old_ifs"

  if [ -n "$added_files" ]; then
    echo "Added $current_dir to PATH in $added_files. Open a new shell to pick it up."
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

if [ "$ALLOW_EXISTING_INSTALL" -ne 1 ] && has_existing_install "$INSTALL_ROOT_RESOLVED"; then
  fail "An existing self-managed Fidan installation was detected at '$INSTALL_ROOT_RESOLVED'. Use 'fidan self install' or re-run bootstrap with --allow-existing-install if you really want to install into the same root."
fi

echo "Fetching manifest from $MANIFEST_URL"
download_to "$MANIFEST_URL" "$MANIFEST_PATH"

RELEASE_INFO="$(read_release_field "$MANIFEST_PATH" "$VERSION" "$HOST_TRIPLE")"
RELEASE_VERSION="$(printf '%s\n' "$RELEASE_INFO" | sed -n '1p')"
ARCHIVE_URL="$(printf '%s\n' "$RELEASE_INFO" | sed -n '2p')"
EXPECTED_SHA="$(printf '%s\n' "$RELEASE_INFO" | sed -n '3p' | tr '[:upper:]' '[:lower:]')"
BINARY_RELPATH="$(printf '%s\n' "$RELEASE_INFO" | sed -n '4p')"

VERSIONS_DIR="$INSTALL_ROOT_RESOLVED/versions"
METADATA_DIR="$INSTALL_ROOT_RESOLVED/metadata"
FINAL_DIR="$VERSIONS_DIR/$RELEASE_VERSION"
CURRENT_LINK="$INSTALL_ROOT_RESOLVED/current"

if [ -e "$FINAL_DIR" ]; then
  fail "Fidan version '$RELEASE_VERSION' is already installed at '$FINAL_DIR'"
fi

mkdir -p "$VERSIONS_DIR" "$METADATA_DIR"
FIRST_INSTALL=1
if [ -d "$VERSIONS_DIR" ] && [ "$(find "$VERSIONS_DIR" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" -gt 0 ]; then
  FIRST_INSTALL=0
fi

echo "Downloading Fidan $RELEASE_VERSION for $HOST_TRIPLE"
download_to "$ARCHIVE_URL" "$ARCHIVE_PATH"
ACTUAL_SHA="$(sha256_of "$ARCHIVE_PATH" | tr '[:upper:]' '[:lower:]')"
if [ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]; then
  fail "SHA-256 mismatch for '$ARCHIVE_URL' (expected $EXPECTED_SHA, got $ACTUAL_SHA)"
fi

tar -xzf "$ARCHIVE_PATH" -C "$EXTRACT_DIR"

CANDIDATE_ROOT="$EXTRACT_DIR"
if [ ! -e "$CANDIDATE_ROOT/$BINARY_RELPATH" ]; then
  set -- "$EXTRACT_DIR"/*
  if [ "$#" -ne 1 ] || [ ! -d "$1" ]; then
    fail "Downloaded archive does not contain '$BINARY_RELPATH' at the root or inside a single top-level directory"
  fi
  CANDIDATE_ROOT="$1"
  if [ ! -e "$CANDIDATE_ROOT/$BINARY_RELPATH" ]; then
    fail "Downloaded archive does not contain the expected file '$BINARY_RELPATH'"
  fi
fi

mv "$CANDIDATE_ROOT" "$FINAL_DIR"
update_metadata "$METADATA_DIR" "$VERSIONS_DIR" "$RELEASE_VERSION" "$FIRST_INSTALL"
if [ "$FIRST_INSTALL" -eq 1 ]; then
  ln -sfn "$FINAL_DIR" "$CURRENT_LINK"
  ensure_path "$CURRENT_LINK"
  echo "Installed Fidan $RELEASE_VERSION and made it active"
else
  echo "Installed Fidan $RELEASE_VERSION"
  echo "Run 'fidan self use $RELEASE_VERSION' to activate it"
fi
echo "Install root: $INSTALL_ROOT_RESOLVED"
