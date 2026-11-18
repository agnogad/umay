#!/usr/bin/env python3
"""Package an already-built UMAY compiler for release.

The build steps belong to the caller: this script only copies the binary and
the documents a user needs, then writes a checksummed archive. It runs on every
host the release jobs use, so Linux and macOS get a tar.gz and Windows gets a
zip.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import shutil
import stat
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


# Fixed timestamps and ownership so a second packaging run of the same inputs
# yields a byte-identical archive (no source-file mtimes leaking into the bytes).
FIXED_DATE_TIME = (1980, 1, 1, 0, 0, 0)  # ZIP's earliest representable time
FIXED_MTIME = 0                          # UNIX epoch, for tar and the gzip header


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
                if not path.is_file():
                    continue
                info = zipfile.ZipInfo(path.relative_to(directory.parent).as_posix())
                info.date_time = FIXED_DATE_TIME
                info.compress_type = zipfile.ZIP_DEFLATED
                info.external_attr = (stat.S_IMODE(path.stat().st_mode) & 0xFFFF) << 16
                with path.open("rb") as src, bundle.open(info, "w") as dst:
                    shutil.copyfileobj(src, dst)
        return archive

    archive = Path(f"{directory}.tar.gz")
    # GzipFile(mtime=0) fixes the gzip header; member metadata is set explicitly
    # below because tarfile.add() would otherwise copy filesystem mtimes/owners.
    with gzip.GzipFile(filename=str(archive), mode="wb", mtime=FIXED_MTIME) as gz:
        with tarfile.open(fileobj=gz, mode="w", format=tarfile.GNU_FORMAT) as bundle:
            def add(path: Path, arcname: str) -> None:
                mode = stat.S_IMODE(path.stat().st_mode)
                info = tarfile.TarInfo(arcname)
                info.mtime = FIXED_MTIME
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                info.mode = mode
                if path.is_dir():
                    info.type = tarfile.DIRTYPE
                    info.size = 0
                    bundle.addfile(info)
                else:
                    info.type = tarfile.REGTYPE
                    info.size = path.stat().st_size
                    with path.open("rb") as src:
                        bundle.addfile(info, src)

            add(directory, directory.name)
            for path in sorted(directory.rglob("*")):
                add(path, f"{directory.name}/{path.relative_to(directory).as_posix()}")
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
