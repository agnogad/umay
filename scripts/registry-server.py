#!/usr/bin/env python3
"""A small UMAY registry server.

The registry is a directory of packages, the same layout `UMAY_REGISTRY` uses
locally. This server adds the HTTP shape `upm` expects:

    GET /<name>/versions.txt          newline-separated versions
    GET /<name>/<version>.tar         the package tree as an USTAR archive

It speaks plain HTTP and is meant to run behind a TLS reverse proxy. The client
downloads with `curl`, so HTTPS works when the URL is HTTPS.
"""

from __future__ import annotations

import argparse
import io
import re
import tarfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

SAFE = re.compile(r"^[A-Za-z0-9._-]+$")


def package_dir(root: Path, name: str, version: str | None = None) -> Path | None:
    if not SAFE.match(name) or name in (".", ".."):
        return None
    path = root / name
    if version is not None:
        if not SAFE.match(version) or version in (".", ".."):
            return None
        path = path / version
    resolved = path.resolve()
    try:
        resolved.relative_to(root.resolve())
    except ValueError:
        return None
    return resolved


def build_tar(directory: Path) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for path in sorted(directory.rglob("*")):
            relative = path.relative_to(directory).as_posix()
            info = archive.gettarinfo(str(path), arcname=relative)
            info.uid = 0
            info.gid = 0
            info.uname = ""
            info.gname = ""
            if path.is_file():
                with path.open("rb") as handle:
                    archive.addfile(info, handle)
            else:
                archive.addfile(info)
    return buffer.getvalue()


def make_handler(root: Path):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):  # noqa: N802 - http.server API
            parts = [unquote(part) for part in urlsplit(self.path).path.split("/") if part]
            if len(parts) == 2 and parts[1] == "versions.txt":
                directory = package_dir(root, parts[0])
                if directory is None or not directory.is_dir():
                    self.send_error(404, "no such package")
                    return
                versions = sorted(p.name for p in directory.iterdir() if p.is_dir())
                body = ("\n".join(versions) + "\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/plain; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            if len(parts) == 2 and parts[1].endswith(".tar"):
                directory = package_dir(root, parts[0], parts[1][:-4])
                if directory is None or not directory.is_dir():
                    self.send_error(404, "no such package version")
                    return
                body = build_tar(directory)
                self.send_response(200)
                self.send_header("Content-Type", "application/x-tar")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            self.send_error(404, "not a registry path")

        def log_message(self, format, *args):  # noqa: A002 - http.server API
            print(f"{self.address_string()} - {format % args}")

    return Handler


def main() -> int:
    parser = argparse.ArgumentParser(description="Serve a directory as a UMAY registry")
    parser.add_argument("--root", required=True, help="registry directory")
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8787)
    args = parser.parse_args()
    root = Path(args.root).resolve()
    if not root.is_dir():
        parser.error(f"not a directory: {root}")
    server = ThreadingHTTPServer((args.bind, args.port), make_handler(root))
    print(f"serving {root} on http://{args.bind}:{server.server_address[1]}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
