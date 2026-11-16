# UMAY Web App Example

A complete web application built with UMAY's standard library (`http`, `json`, `static_files`, `cookies`, `sqlite`, `template`, `server_config`, `time`).

## Build and Run

```bash
cd examples/web-app && umay build main.um && ./dist/main
```

Then visit `http://127.0.0.1:8080`.

## Environment Variables

- `PORT` (default `8080`): TCP port to listen on.
- `APP_DB` (default `app.db`): SQLite database path.
- `APP_SECRET`: HMAC key used to sign session cookies. If unset, the server generates a random 32-byte hex key at startup, warns on stderr that sessions will not survive a restart, and stores it via `setenv` so forked request workers share it.
- `APP_PBKDF2_ITERATIONS` (default `100000`): PBKDF2-HMAC-SHA256 iteration count for password hashing (lower values such as `1000` are useful in tests).
- `UMAY_LOG` (set to `off` to disable): Controls per-request logging on stderr.

## Routes

- `GET /` — Home page (greeting + notes when signed in, sign-up/log-in links otherwise).
- `GET /signup`, `POST /signup` — Account registration (requires non-empty email containing `@` and password of at least 8 characters; duplicate email returns `409`).
- `GET /login`, `POST /login` — Sign in with email and password (`401` on invalid credentials).
- `POST /logout` — Clear the session cookie (`Max-Age=0`) and redirect to `/`.
- `GET /api/notes` — List notes for the signed-in user as JSON (`401` without a valid session).
- `POST /api/notes` — Create a note from form field `text` (`1..=1000` bytes; `201 {"id":N}`, `422` on invalid input, `401` without session).
- `DELETE /api/notes/:id` — Delete one of the signed-in user's notes (`204` on delete, `404` if missing or owned by another user, `401` without session).
- `GET /static/*` — Serve static assets from `public/`.
