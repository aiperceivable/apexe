#!/bin/sh
# Install apexe from a GitHub release and prepare this machine.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/aiperceivable/apexe/main/scripts/install.sh | sh
#   curl -fsSL ... | sh -s -- --version v0.15.0
#   sh scripts/install.sh --dry-run
#
# Options:
#   --version <tag>    Release tag to install (default: the newest release)
#   --prefix <dir>     Install directory (default: ~/.local/bin); never uses sudo
#   --no-setup         Install the binary and stop
#   --dry-run          Print the resolved download and install plan
#   --yes              Pass to 'apexe setup': configure detected hosts unprompted
#   --no-hosts         Pass to 'apexe setup': prepare rules and shims only
#   --offline          Pass to 'apexe setup': use cached rules
#   --force            Pass to 'apexe setup': replace a same-named corpus
#   --overlays <src>   Pass to 'apexe setup': rules repository, checkout or archive
#   --ref <ref>        Pass to 'apexe setup': Git branch, tag or commit
#   -h, --help         Show this message
#
# This script installs a binary and hands off. Everything about rules, ACL,
# shims and agent hosts belongs to 'apexe setup', so the behaviour does not
# freeze at the version of this script a user happened to download.
set -eu

REPOSITORY="${APEXE_RELEASE_REPOSITORY:-https://github.com/aiperceivable/apexe}"
VERSION=""
PREFIX="${HOME}/.local/bin"
RUN_SETUP=1
DRY_RUN=0
SETUP_YES=0
SETUP_NO_HOSTS=0
SETUP_OFFLINE=0
SETUP_FORCE=0
SETUP_OVERLAYS=""
SETUP_REF=""

usage() {
    awk 'NR > 1 && /^set -eu$/ { exit } NR > 1 { sub(/^# ?/, ""); print }' "$0"
}

fail() {
    printf 'apexe install: %s\n' "$1" >&2
    exit 1
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || fail "--version requires a tag"
            VERSION="$2"
            shift 2
            ;;
        --prefix)
            [ "$#" -ge 2 ] || fail "--prefix requires a directory"
            PREFIX="$2"
            shift 2
            ;;
        --overlays)
            [ "$#" -ge 2 ] || fail "--overlays requires a source"
            SETUP_OVERLAYS="$2"
            shift 2
            ;;
        --ref)
            [ "$#" -ge 2 ] || fail "--ref requires a branch, tag or commit"
            SETUP_REF="$2"
            shift 2
            ;;
        --no-setup) RUN_SETUP=0; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --yes) SETUP_YES=1; shift ;;
        --no-hosts) SETUP_NO_HOSTS=1; shift ;;
        --offline) SETUP_OFFLINE=1; shift ;;
        --force) SETUP_FORCE=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *)
            printf 'apexe install: unknown option %s\n' "$1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "${os}:${arch}" in
        Darwin:arm64) echo "aarch64-apple-darwin" ;;
        Darwin:x86_64) echo "x86_64-apple-darwin" ;;
        Linux:x86_64|Linux:amd64) echo "x86_64-unknown-linux-gnu" ;;
        Linux:aarch64|Linux:arm64) echo "aarch64-unknown-linux-gnu" ;;
        *)
            fail "unsupported platform ${os}/${arch}; build from source instead: cargo install --path ."
            ;;
    esac
}

archive_url() {
    if [ -n "${VERSION}" ]; then
        echo "${REPOSITORY}/releases/download/${VERSION}/apexe-${TARGET}.tar.gz"
    else
        echo "${REPOSITORY}/releases/latest/download/apexe-${TARGET}.tar.gz"
    fi
}

# The setup command line, both for display and for execution. Keeping it in one
# place means the dry run cannot drift from what the script actually runs.
setup_arguments() {
    if [ "${SETUP_YES}" -eq 1 ]; then echo "--yes"; fi
    if [ "${SETUP_NO_HOSTS}" -eq 1 ]; then echo "--no-hosts"; fi
    if [ "${SETUP_OFFLINE}" -eq 1 ]; then echo "--offline"; fi
    if [ "${SETUP_FORCE}" -eq 1 ]; then echo "--force"; fi
    if [ -n "${SETUP_OVERLAYS}" ]; then
        echo "--overlays"
        echo "${SETUP_OVERLAYS}"
    fi
    if [ -n "${SETUP_REF}" ]; then
        echo "--ref"
        echo "${SETUP_REF}"
    fi
}

TARGET="$(target)" || exit 1
URL="$(archive_url)"

if [ "${DRY_RUN}" -eq 1 ]; then
    printf 'Platform: %s\n' "${TARGET}"
    printf 'Download: %s\n' "${URL}"
    printf 'Verify:   %s.sha256\n' "${URL}"
    printf 'Install:  %s/apexe\n' "${PREFIX}"
    if [ "${RUN_SETUP}" -eq 1 ]; then
        printf 'Then:     %s/apexe setup' "${PREFIX}"
        setup_arguments | while IFS= read -r argument; do
            printf ' %s' "${argument}"
        done
        printf '\n'
    fi
    exit 0
fi

command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 \
    || fail "curl or wget is required"

if command -v shasum >/dev/null 2>&1; then
    SHA_CMD="shasum -a 256"
elif command -v sha256sum >/dev/null 2>&1; then
    SHA_CMD="sha256sum"
else
    fail "shasum or sha256sum is required to verify the download"
fi

STAGING="$(mktemp -d)" || fail "cannot create a temporary directory"
trap 'rm -rf "${STAGING}"' EXIT INT TERM

download() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2" || fail "download failed: $1"
    else
        wget -q "$1" -O "$2" || fail "download failed: $1"
    fi
}

ARCHIVE="${STAGING}/apexe-${TARGET}.tar.gz"
printf 'Downloading %s\n' "${URL}"
download "${URL}" "${ARCHIVE}"
download "${URL}.sha256" "${ARCHIVE}.sha256"

( cd "${STAGING}" && ${SHA_CMD} -c "apexe-${TARGET}.tar.gz.sha256" >/dev/null ) \
    || fail "checksum verification failed for ${URL}"

tar -xzf "${ARCHIVE}" -C "${STAGING}" || fail "cannot unpack ${URL}"
BINARY="${STAGING}/apexe-${TARGET}/apexe"
[ -f "${BINARY}" ] || fail "the archive does not contain apexe-${TARGET}/apexe"

mkdir -p "${PREFIX}" || fail "cannot create ${PREFIX}"
if [ ! -w "${PREFIX}" ]; then
    fail "${PREFIX} is not writable; choose another --prefix or fix its permissions (this script never uses sudo)"
fi
install -m 0755 "${BINARY}" "${PREFIX}/apexe" || fail "cannot install into ${PREFIX}"

if ! "${PREFIX}/apexe" --version >/dev/null 2>&1; then
    fail "${PREFIX}/apexe does not run; on Linux this usually means an older glibc than the release was built against"
fi
printf 'Installed %s\n' "$("${PREFIX}/apexe" --version)"

case ":${PATH}:" in
    *":${PREFIX}:"*) ;;
    *)
        printf '\n%s is not on PATH. Add it, then restart your shell:\n' "${PREFIX}"
        printf '  export PATH="%s:$PATH"\n' "${PREFIX}"
        ;;
esac

if [ "${RUN_SETUP}" -eq 0 ]; then
    printf '\nNext: %s/apexe setup\n' "${PREFIX}"
    exit 0
fi

# Rebuild the argument vector with quoting intact: a source path may contain
# spaces, and a single string would split it.
set -- setup
if [ "${SETUP_YES}" -eq 1 ]; then set -- "$@" --yes; fi
if [ "${SETUP_NO_HOSTS}" -eq 1 ]; then set -- "$@" --no-hosts; fi
if [ "${SETUP_OFFLINE}" -eq 1 ]; then set -- "$@" --offline; fi
if [ "${SETUP_FORCE}" -eq 1 ]; then set -- "$@" --force; fi
if [ -n "${SETUP_OVERLAYS}" ]; then set -- "$@" --overlays "${SETUP_OVERLAYS}"; fi
if [ -n "${SETUP_REF}" ]; then set -- "$@" --ref "${SETUP_REF}"; fi

printf '\nPreparing rules, shims and agent hosts (the first run may download the rules corpus).\n'

# Hand off to 'apexe setup' with the terminal attached when there is one, and
# with the non-interactive pipe otherwise, where setup writes no host config.
#
# The redirection belongs to the handoff command, never to the shell: under
# `curl | sh` this script is the shell's stdin, so rebinding the shell's own
# stdin makes it look for the rest of the script on the terminal and hang.
# That is why the handoff is also the last statement in this file.
if [ -t 0 ]; then
    exec "${PREFIX}/apexe" "$@"
elif (exec </dev/tty) 2>/dev/null; then
    exec "${PREFIX}/apexe" "$@" </dev/tty
else
    exec "${PREFIX}/apexe" "$@"
fi
