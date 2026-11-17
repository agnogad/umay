# ADR 0016: HTTP(S) `upm` registry

**Status:** accepted

## Context

`upm` fetched dependencies only from directories named by `UMAY_REGISTRY`. That
kept version choice and content hashing simple and testable, but a project could
not depend on a package published once for many machines. The missing piece was
transport, not resolution: a hosted registry serves the same package tree.

## Decision

A registry root is either a directory or an HTTP(S) base URL. URL roots are
listed with `;`, so a colon in `http://` is not mistaken for the platform path
separator:

```sh
UMAY_REGISTRY="https://registry.example/;https://mirror.example/;/opt/private"
```

A URL root serves:

```text
GET <base>/<name>/versions.txt     newline-separated versions
GET <base>/<name>/<version>.tar    package tree as uncompressed USTAR
```

The client downloads with `curl`, which is already present on the systems UMAY
targets and carries TLS without adding a cryptography dependency to the
compiler. Missing packages fall through to the next root. Extraction rejects
absolute paths and `..`, writes only regular files and directories, and checks
for `umay.toml` before accepting the package. The lockfile still records the
content hash of what was extracted, so transport does not weaken the build
check.

`scripts/registry-server.py --root <dir>` serves a directory in this shape. It
speaks plain HTTP and is meant to run behind a TLS reverse proxy.

## Consequences

- A URL registry needs `curl` on `PATH`; a directory registry does not.
- The server can generate tarballs from the same directory layout `upm`
  already understands, so there is no second package format.
- Package signing is not part of this decision. The lockfile pins content, but
  first fetch trusts the transport and the registry.
- A registry still cannot express a version range; the client chooses the
  newest version when the manifest names none.

## Alternatives considered

- **A JSON file list per version.** It needs several round trips and a second
  parser on the client for no gain over one archive.
- **Serve package directories directly.** It exposes file-by-file fetches and
  lets a half-updated directory be observed. One archive per version is atomic.
- **Git as transport.** Correct and authenticated, but cloning a repository for
  one package is heavy, and it makes `git` a runtime requirement.
- **A TLS implementation inside the compiler.** That adds a large dependency
  and a certificate store to maintain for a feature `curl` already provides.
