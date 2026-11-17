# Security policy

## Supported versions

Security fixes are applied to the latest released version. At the time of
writing that is `0.3.x`. Older releases are not maintained.

## Reporting a vulnerability

Do not open a public issue for a security problem.

Use GitHub's private vulnerability reporting:

1. Open the repository's **Security** tab.
2. Choose **Report a vulnerability**.
3. Include the UMAY version (`umay version`), the host system and target
   triple, a minimal reproducing program, and the command that triggers it.

If private reporting is unavailable, contact the repository owner
([@agnogad](https://github.com/agnogad)) through GitHub and ask for a private
channel. Do not include exploit details in a public message.

You can expect an acknowledgement within a few days. A fix and a coordinated
disclosure will be arranged before any public write-up.

## What counts as a security issue

- A valid UMAY program that is silently miscompiled in a way that violates
  memory safety.
- A compiler crash that can be turned into code execution through build inputs.
- Path traversal or arbitrary file writes in `umay new`, `umay clean`, the
  cache, or `upm` package fetching.
- A release artifact whose checksum does not match the published one.

A program that uses `unsafe`, raw pointers, or `extern fn` incorrectly is not
a compiler vulnerability; the contract it breaks is documented in
`docs/memory-model.md`.
