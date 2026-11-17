#!/bin/sh
# Installs a prebuilt UMAY compiler on Linux or Intel macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/agnogad/umay/main/install.sh | sh
#
# Set UMAY_VERSION to install a specific release, UMAY_INSTALL_DIR to choose the
# destination (default: ~/.local/bin), or UMAY_BASE_URL to download from a mirror.
# Windows users should download the x86_64-windows zip from the release page.

set -eu

repo="${UMAY_REPO:-agnogad/umay}"
install_dir="${UMAY_INSTALL_DIR:-${HOME}/.local/bin}"
api="${UMAY_API:-https://api.github.com/repos/${repo}/releases/latest}"

fail() {
    echo "umay-install: $*" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || fail "`$1` is required"
}

need curl
need tar
need uname

os_name="${UMAY_OS:-$(uname -s)}"
arch_name="${UMAY_ARCH:-$(uname -m)}"
case "${os_name}-${arch_name}" in
    Linux-x86_64 | Linux-amd64) label="x86_64-linux-musl" ;;
    Linux-aarch64 | Linux-arm64) label="aarch64-linux-musl" ;;
    Darwin-x86_64 | Darwin-amd64) label="x86_64-darwin" ;;
    Darwin-aarch64 | Darwin-arm64)
        fail "ARM64 macOS artifacts are not published yet; build from source" ;;
    *)
        fail "unsupported system: ${os_name} ${arch_name}; use the release archive for your system"
        ;;
esac

version="${UMAY_VERSION:-}"
if [ -z "$version" ]; then
    version="$(curl -fsSL "$api" | sed -n 's/.*"tag_name":[[:space:]]*"v\([^"]*\)".*/\1/p' | head -n 1)"
    [ -n "$version" ] || fail "could not find the latest release; set UMAY_VERSION"
fi

base="${UMAY_BASE_URL:-https://github.com/${repo}/releases/download/v${version}}"
archive="umay-${version}-${label}.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "${base}/${archive}" -o "${tmp}/${archive}" ||
    fail "cannot download ${base}/${archive}"
curl -fsSL "${base}/${archive}.sha256" -o "${tmp}/${archive}.sha256" ||
    fail "cannot download the checksum for ${archive}"

(
    cd "$tmp"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "${archive}.sha256"
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -c "${archive}.sha256"
    else
        fail "sha256sum or shasum is required to verify the download"
    fi
)

tar -xzf "${tmp}/${archive}" -C "$tmp"
mkdir -p "$install_dir"
install -m 0755 "${tmp}/umay-${version}-${label}/umay" "${install_dir}/umay"

if [ "${UMAY_VERIFY:-1}" = "1" ]; then
    "${install_dir}/umay" version
fi
case ":${PATH}:" in
    *":${install_dir}:"*) ;;
    *) echo "Add ${install_dir} to PATH to run \`umay\` from anywhere." ;;
esac
