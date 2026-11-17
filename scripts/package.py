#!/usr/bin/env python3
"""Package an already-built UMAY compiler for release.

The build steps belong to the caller: this script only copies the binary and
the documents a user needs, then writes a checksummed archive. It runs on every
host the release jobs use, so Linux and macOS get a tar.gz and Windows gets a
zip.
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
import subprocess
import tarfile
import zipfile
from pathlib import Path

DOCUMENTS = [
    "README.md",
    "CHANGELOG.md",
    "docs/language-specification.md",
    "docs/build-system.md",
    "scripts/registry-server.py",
    "LICENSE",
]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def archive_directory(directory: Path, windows: bool) -> Path:
    if windows:
        archive = Path(f"{directory}.zip")
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as bundle:
            for path in sorted(directory.rglob("*")):
                if path.is_file():
                    bundle.write(path, path.relative_to(directory.parent).as_posix())
        return archive

    archive = Path(f"{directory}.tar.gz")
    with tarfile.open(archive, "w:gz") as bundle:
        bundle.add(directory, arcname=directory.name)
    return archive


def main() -> int:
    parser = argparse.ArgumentParser(description="Package a built umay compiler")
    parser.add_argument("--version", required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--out", default="out")
    parser.add_argument("--strip", action="store_true")
    args = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    binary = Path(args.binary)
    if not binary.is_absolute():
        binary = root / binary
    if not binary.is_file():
        parser.error(f"binary not found: {binary}")

    out = Path(args.out)
    if not out.is_absolute():
        out = root / out
    out.mkdir(parents=True, exist_ok=True)

    windows = "windows" in args.label.lower()
    directory = out / f"umay-{args.version}-{args.label}"
    shutil.rmtree(directory, ignore_errors=True)
    directory.mkdir(parents=True)

    executable = "umay.exe" if windows else "umay"
    packaged = directory / executable
    shutil.copy2(binary, packaged)
    if args.strip and not windows:
        # GNU `strip` is often built for the host's own file formats; llvm-strip
        # handles a cross-built object without a cross binutils package.
        for tool in ("llvm-strip", "strip"):
            if shutil.which(tool):
                subprocess.run(
                    [tool, str(packaged)], check=False, stderr=subprocess.DEVNULL
                )
                break

    for name in DOCUMENTS:
        source = root / name
        if source.is_file():
            shutil.copy2(source, directory / source.name)

    archive = archive_directory(directory, windows)
    checksum = f"{sha256(archive)}  {archive.name}\n"
    checksum_path = archive.with_name(f"{archive.name}.sha256")
    checksum_path.write_text(checksum, encoding="utf-8")

    print(archive)
    print(checksum, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
