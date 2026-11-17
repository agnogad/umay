# Web backend plan

Goal: a real website backend (routes, JSON API, forms, static files, sessions,
database) written in UMAY with nothing but its stdlib and libc.

What already exists: TCP sockets (`net`), a request-line/header/body parser and
`Response` (`http`), a JSON reader and writer helpers (`json`), files (`fs`),
`fork`/`spawn` and threads, `Vec`, `HashMap`, `String`, `Bytes`, function
pointers (`fn(A) -> B`).

TLS is out of scope: the server speaks plain HTTP behind a reverse proxy
(Caddy/nginx) that terminates TLS.

## Parts

Each part lands with a test in `compiler/tests/web_backend.rs` that compiles
and runs a program, the same way `phase6_runtime.rs` does.

1. **HTTP core** (done) — header lookup by name, path/query split, query parameter
   lookup, percent-decoding, a full read loop (head + `Content-Length` body
   across several `read`s, with a size cap), a response writer with
   `Content-Type` and extra headers, the common status codes.
2. **Router** (done) — method + path table of `fn(&Request, &mut Response)` handlers
   (out-parameter: an indirect call cannot return more than 16 bytes yet),
   `:param` path segments, 404/405, a `serve(port, routes)` loop with one
   child per connection (fork, already used by `examples/http-router.um`).
3. **Bodies** (done) — `JsonOut` builder, `req.form(name)` and `req.query_param(name)`
   (decoded); `application/x-www-form-urlencoded`
   parsing. Multipart uploads only when something needs them.
4. **Static files** (done) — serve a directory, MIME by extension, reject `..`.
5. **Time and randomness** (done) — `clock_gettime`, HTTP `Date`, `getrandom` for
   tokens.
6. **Crypto** (done) — SHA-256, HMAC-SHA-256 (signed cookies), PBKDF2 (passwords),
   constant-time compare. Verified against the RFC/NIST test vectors.
7. **Cookies and sessions** (done; `stdlib/cookies.um`, also password hashes) — `Cookie` parsing, `Set-Cookie` writer, signed
   session id cookie.
8. **Database** (done; `stdlib/sqlite.um`) — SQLite through `extern fn`. The
   compiler adds `-lsqlite3` when the program calls an `sqlite3_*` symbol
   (`Target::system_libraries`), so no manifest key or flag is needed and a
   program that does not use it never needs the library.
9. **Templates** (done; `stdlib/template.um`) — `{{name}}` substitution with HTML escaping.
10. **Operations** (done; `stdlib/server_config.um`, `stdlib/platform/ops_*.um`)
    — `env_or`/`env_port`, a stderr request log (`UMAY_LOG=off` disables it),
    `SO_REUSEADDR`, and graceful shutdown on SIGTERM/SIGINT (signals blocked,
    `poll` with a 250 ms timeout, in-flight children awaited; Linux and macOS
    only). HTTP/1.1 keep-alive is on: a connection serves its requests in one
    child, HTTP/1.0 requests close as the protocol says, and an idle connection
    closes after a second. Nagle is off and a body that fits the head's buffer
    is appended to it, so a response leaves in one write -- with Nagle on, the
    second small write waited for the peer's delayed acknowledgement and every
    keep-alive response cost about 40 ms.

Order is the dependency order: 1 → 2 → 3/4 → 5 → 6 → 7; 8 and 9 are
independent after 2. The capstone is an example app (`examples/web-app.um`):
sign-up/login, a JSON API over SQLite, and static pages.
