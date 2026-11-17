# Releasing UMAY

A release is a promise that a commit builds, passes its tests, and matches its
documentation. The steps below exist so that promise is checked rather than
assumed.

## Versions

Two versions move independently:

- **The compiler's package version** (`Cargo.toml`, printed by `umay version`)
  follows semantic versioning. It moves for any change, including one that only
  touches the implementation.
- **The language specification version** (`docs/language-specification.md`)
  moves only when the language surface changes. It is `1.0` and should stay there
  until the language itself changes in a breaking way.

## Checklist

1. **The tree is clean.** `scripts/release.sh` refuses to run on a dirty tree,
   because the artifact would not correspond to any commit.

2. **The specification is not a draft.** The script also refuses this. Promoting
   the spec is a deliberate act, not a side effect of cutting a release.

3. **The tests pass.** The script runs `cargo test` and stops on any failure. A
   release that fails its own suite is worse than no release.

4. **The changelog is written.** Add a section to `CHANGELOG.md` under the new
   version, describing what changed from a user's point of view: language,
   compiler, standard library, toolchain, and any known gaps. The gaps matter as
   much as the features -- a reader needs to know what does not work.

5. **Bump the version** in `Cargo.toml` if the changelog section is for a version
   other than the current one.

6. **Build the artifact:**

   ```sh
   scripts/release.sh
   ```

   This produces `out/umay-<version>-<host>.tar.gz`, its `.sha256`, and a
   stripped binary inside. Nothing is written into a project's `dist/`.

7. **Tag the commit:**

   ```sh
   git tag -a v<version> -m "umay <version>"
   git push origin v<version>
   ```

8. **The GitHub release publishes itself.** Pushing the tag builds the
   x86_64 Linux, AArch64 Linux, macOS and Windows artifacts. The Linux jobs
   check that their binaries are static. A final job attaches every archive and
   checksum to the tag. A user should be able to download a pair and run
   `sha256sum -c` before executing anything. If the jobs could not publish,
   attach the files by hand rather than leaving the tag with no download.

## What the artifact contains

- `umay` on Linux and macOS, `umay.exe` on Windows — the compiler. Linux,
  AArch64 Linux and macOS builds are stripped.
- `README.md`, `CHANGELOG.md` — what it is and what changed.
- `language-specification.md`, `build-system.md` — the normative documents.
- `LICENSE`.

Linux (x86_64 and AArch64) and macOS use a `tar.gz`; Windows uses a `zip`. Each
archive has a sibling `.sha256` file.

It does **not** contain the Cargo build directory, the source tree, or any
project's `dist/`. On Linux the compiler is built against musl and statically
linked, so it does not depend on the host's glibc. On other systems it links the
system runtime the way a native binary normally does.

## Installing

There is no installer yet, and that is honest rather than forgotten: unpack the
tarball and put `umay` on the path.

```sh
tar xzf umay-<version>-x86_64-linux-musl.tar.gz
install -m 0755 umay-<version>-x86_64-linux-musl/umay ~/.local/bin/umay
umay version
```

On Windows, unpack the `x86_64-windows` zip and put `umay.exe` on `PATH`.

## After the release

Record the release's headline numbers in `docs/measurements.md` if they changed
in a way a reader should know about (build time, binary size). The measurements
are not a gate, but a release is when it is cheapest to refresh them.
