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

8. **Attach the tarball and its checksum** to the tag's release page. A user
   should be able to download both and run `sha256sum -c` before executing
   anything.

## What the artifact contains

- `umay` — the compiler, stripped.
- `README.md`, `CHANGELOG.md` — what it is and what changed.
- `language-specification.md`, `build-system.md` — the normative documents.
- `LICENSE`, when the repository has one.

It does **not** contain the Cargo build directory, the source tree, or any
project's `dist/`. The compiler is a single static binary that links only libc.

## Installing

There is no installer yet, and that is honest rather than forgotten: unpack the
tarball and put `umay` on the path.

```sh
tar xzf umay-<version>-<host>.tar.gz
install -m 0755 umay-<version>-<host>/umay ~/.local/bin/umay
umay version
```

## After the release

Record the release's headline numbers in `docs/measurements.md` if they changed
in a way a reader should know about (build time, binary size). The measurements
are not a gate, but a release is when it is cheapest to refresh them.
