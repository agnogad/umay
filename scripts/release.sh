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

# A glibc build only runs on a host at least as new as the one that produced it.
# Build the Linux release against musl instead, where the target is installed:
# the result is static and runs on every Linux distribution. `UMAY_RELEASE_TARGET`
# overrides the choice for a developer building for a different system.
default_target=""
if [ "$(uname -s)" = "Linux" ] && rustup target list --installed 2>/dev/null | grep -qx "x86_64-unknown-linux-musl"; then
    default_target="x86_64-unknown-linux-musl"
fi
target="${UMAY_RELEASE_TARGET:-$default_target}"
if [ "$target" = "x86_64-unknown-linux-musl" ]; then
    host="x86_64-linux-musl"
fi
host="${UMAY_RELEASE_LABEL:-$host}"

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

# 3. The suite and the lints must pass. A release is a claim that the tree is
# clean in both senses.
echo "==> checking lints"
cargo clippy --all-targets --quiet -- -D warnings

echo "==> running the test suite"
cargo test --quiet

# 4. Build the compiler at the release profile.
echo "==> building"
if [ -n "$target" ]; then
    cargo build --release --quiet --target "$target"
    binary="target/${target}/release/umay"
else
    cargo build --release --quiet
    binary="target/release/umay"
fi
[ -x "$binary" ] || { echo "error: $binary was not produced" >&2; exit 1; }

# 5. Package and checksum. One script does this on every host, so the
# documents and the checksum layout cannot drift between systems.
echo "==> packaging"
archive="$(python3 scripts/package.py \
    --version "$version" \
    --label "$host" \
    --binary "$binary" \
    --out "$out_dir" \
    --strip | head -n1)"

# 6. On an exact tag only, verify the archive is byte-reproducible by packaging
# the same binaries a second time into a scratch dir and comparing the two
# checksum files. A second full `cargo build --release` would be the stronger
# check but costs another multi-minute build on every tag; package.py now pins
# gzip/zip timestamps and member modes, so two packaging runs of identical
# inputs exercise the determinism the release claim rests on. This is a tag
# check only -- it never runs on (and never gates) a PR.
if git describe --exact-match --tags HEAD >/dev/null 2>&1; then
    echo "==> verifying reproducible packaging (tag build)"
    verify_out="$(mktemp -d)"
    second_archive="$(python3 scripts/package.py \
        --version "$version" \
        --label "$host" \
        --binary "$binary" \
        --out "$verify_out" \
        --strip | head -n1)"
    if cmp -s "$archive.sha256" "$second_archive.sha256"; then
        echo "==> archives are byte-identical across packaging runs"
    else
        echo "error: archive is not reproducible across packaging runs" >&2
        echo "  $archive" >&2
        echo "  $second_archive" >&2
        rm -rf "$verify_out"
        exit 1
    fi
    rm -rf "$verify_out"
fi

echo
echo "verify with:  sha256sum -c <archive>.sha256"
