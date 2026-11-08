#!/usr/bin/env bash
#
# Builds a release artifact for the UMAY compiler.
#
# The output is a stripped, checksummed tarball of the `umay` binary plus the
# documents a user needs (specification, changelog, readme, license). It is built
# from the `release` Cargo profile and never writes into a project's `dist/`,
# because the no-`target/`-in-a-project rule is about projects, not about the
# compiler's own packaging.
#
# Usage:
#     scripts/release.sh [VERSION]
#
# VERSION defaults to the version in `Cargo.toml`. The script refuses to run
# unless the working tree is clean, the test suite passes, and the specification
# is no longer a draft -- a release built from a failed tree or a draft spec is
# not a release.

set -euo pipefail

cd "$(dirname "$0")/.."

version="${1:-$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')}"
tag="v${version}"
out_dir="out"
host="$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"

echo "==> releasing umay ${tag} for ${host}"

# 1. A dirty tree cannot be released: the artifact would not match a commit.
if [ -n "$(git status --porcelain)" ]; then
    echo "error: the working tree is not clean; commit or stash first" >&2
    git status --short >&2
    exit 1
fi

# 2. The specification must not be a draft.
if grep -q "Status: \*\*draft\*\*" docs/language-specification.md; then
    echo "error: the language specification is still marked draft" >&2
    exit 1
fi

# 3. The suite must pass. A release is a claim that the tests pass.
echo "==> running the test suite"
cargo test --quiet

# 4. Build the compiler at the release profile.
echo "==> building"
cargo build --release --quiet
binary="target/release/umay"
[ -x "$binary" ] || { echo "error: $binary was not produced" >&2; exit 1; }

# 5. Assemble the package.
pkg="${out_dir}/umay-${version}-${host}"
rm -rf "$pkg"
mkdir -p "$pkg"
cp "$binary" "$pkg/umay"
strip "$pkg/umay" 2>/dev/null || true

for doc in README.md CHANGELOG.md docs/language-specification.md docs/build-system.md; do
    [ -f "$doc" ] && cp "$doc" "$pkg/$(basename "$doc")"
done
[ -f LICENSE ] && cp LICENSE "$pkg/LICENSE"

# 6. Checksum, so a download can be checked rather than trusted.
tarball="${pkg}.tar.gz"
tar -C "$out_dir" -czf "$tarball" "$(basename "$pkg")"
( cd "$out_dir" && sha256sum "$(basename "$tarball")" > "$(basename "$tarball").sha256" )

echo "==> wrote $tarball"
echo "    $(cat "${tarball}.sha256")"
echo
echo "verify with:  sha256sum -c $(basename "$tarball").sha256"
