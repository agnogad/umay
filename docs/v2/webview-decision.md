# Webview backend decision (v2 Phase 5 desktop, Phase 6 mobile)

**Status:** v2 design decision. Not implemented. ADR 0020 owns the GUI backend
strategy (webview vs native widgets); ADR 0021 owns the mobile shell and
packaging. This document decides the *webview runtime* per OS and the shared
bridge, and constrains those ADRs: both must use the IPC and asset scheme below.

## 1. Decision

One app UI (HTML/CSS/JS), one IPC protocol, three native adapters:

| OS | Webview runtime | Notes |
|---|---|---|
| Windows (x64 + ARM64) | **WebView2** (Chromium, Evergreen runtime) | Ships as a runtime, not part of Windows; ARM64 supported, matching the `windows-arm` CI job |
| macOS + iOS | **WKWebView** | OS-provided; the single engine serves desktop and mobile (ADR 0021) |
| Linux | **WebKitGTK** | Distributed as a system library; resolves through the native-dependency model |

Rejected: Electron/Chromium bundling (hundreds of MB, own update cycle), a custom
JS engine (reimplementing a browser), one engine everywhere (no supported engine
is native on all three OSes), native-widget UI for this phase (ADR 0020 decides
that separately; if it is chosen for a screen, that screen does not use this
bridge).

This decides the webview backend only. ADR 0020 puts SDL3 first at the
`umay-gui` facade and the webview second; the order below is **within that
second backend**. ADR 0021 owns Android's WebView and the mobile shell; this
document does not revisit it.

**Implementation order within the webview backend: Linux → macOS → Windows.**

1. Linux/WebKitGTK first: `ubuntu-latest` installs it with `apt`, the loop is
   fastest, and it forces a clean scheme-handler/bridge design from the start.
2. macOS/WKWebView second: one implementation covers macOS and iOS (ADR 0021),
   and CI has a real Intel Mac runner (`ci.yml:22-53`); Apple Silicon is
   cross-built only (`ci.yml:153-206`), so it is validated by the AArch64 Linux
   and Windows ARM64 jobs rather than a native Mac here.
3. Windows/WebView2 last: it needs runtime detection and a bootstrap path, and
   it is the only adapter with two architectures to verify. The `windows` and
   `windows-arm` jobs (`ci.yml:208-314`) cover both.

This order front-loads the adapter with the fewest proprietary hooks and leaves
the runtime-availability problem for last, when the bridge is already fixed.

## 2. One shared IPC design

Same protocol on all three runtimes; adapters only transport UTF-8 strings.

- Injected JS global: `window.umay.send(json: string)` → native, and
  `window.umay.onMessage(cb)` ← native. No other JS↔native surface.
- Envelope (JSON, parsed with `stdlib/json.um`):
  - call: `{"id":n,"method":"name","params":{...}}`
  - reply: `{"id":n,"ok":true,"result":{...}}` or
    `{"id":n,"ok":false,"error":{"code":..,"message":".."}}`
  - event: `{"event":"name","params":{...}}` (no id)
- `id` is a monotonic native-side counter; JS echoes it. No shared memory, no
  per-backend message format, no generated per-method JS bindings in v2.
- One connection per webview instance. Each window registers an allowlist of
  method names; methods are ordinary UMAY functions mapped by name in the
  shell. There is no reflection into UMAY.

## 3. Asset serving and local resources

- App resources are served under a custom scheme `umay://app/...`, **not**
  `file://` and not `http(s)://`. Registered per backend:
  - WebView2: `WebResourceRequested` + `AddWebResourceRequestedFilter`,
  - WKWebView: `WKURLSchemeHandler` (`setURLSchemeHandler:forURLScheme:`),
  - WebKitGTK: `webkit_web_context_register_uri_scheme`.
- The scheme handler maps the path to a file in the bundle produced by
  `umay bundle` and resolved through the ADR 0020 asset API, not the process CWD.
  Path is confined to the bundle root; `..`,
  absolute paths and symlinks escaping the root are rejected. MIME by extension.
- The same bundle (same bytes, same relative paths) is shipped on every OS. No
  platform-specific HTML and no build-time HTML rewriting.
- v2 allows only `umay://app/` and `about:blank`. Remote navigation is blocked;
  allowing a remote origin is a later decision with its own CSP policy.
- A restrictive Content Security Policy is set on every page:
  `default-src 'self' umay:`, no inline scripts, no `eval`, no `data:` URLs.

## 4. Security boundary and message validation

The webview is untrusted input, including JS from the app's own bundle (a bug or
a compromised asset is the threat model).

- Parse every inbound message as JSON; cap size (1 MiB in v2) before parsing;
  reject malformed JSON with a reply error, never a native panic.
- Validate `method` against the per-window allowlist, `params` against a schema
  (types, required fields, string/array length caps) and `id` as an integer.
  Unknown methods and unknown fields are rejected, not ignored.
- No native paths, handles, file descriptors or capabilities cross the bridge
  as data. UMAY-side handlers decide what a method may do.
- Native→JS uses `postMessage`/`CallScriptMethod` with a JSON string argument.
  Never build a JS source string from native data (no `eval`, no string
  interpolation into script).
- Capability scope: each window registers only the methods that window needs;
  a webview for a settings dialog does not get the application's full method
  set. Rate limiting and a user-facing permission prompt are deferred.
- Assets are content-hashed into the build (native-dependencies build model), so
  a changed UI is a changed build, not a silent runtime fetch.

## 5. Native shell code

Each adapter is a small native shim exposing a stable C ABI that UMAY calls via
`extern fn` (see [ffi-spike.md](ffi-spike.md) for callbacks and raw pointers). It
owns: window creation, the run loop, the message bridge, the scheme handler, and
a callback into UMAY for inbound messages.

- Windows: C against WebView2's Win32 COM interfaces (`CreateWebView2Environment`,
  `ICoreWebView2`, `add_WebMessageReceived`).
- macOS/iOS: Objective-C/Swift against WKWebView (`WKWebView`,
  `WKScriptMessageHandler`, `WKURLSchemeHandler`).
- Linux: C against WebKitGTK's GObject API.

The shim is compiled by the target C/ObjC compiler and linked by the build; its
sources and flags are declared through the native-dependency/build-script model
([native-dependencies.md](native-dependencies.md)). UMAY declares the shim's
functions in a v2 `extern "C"` block and calls them inside `unsafe`
(`docs/v2/specification.md:87-111`), and passes an `extern "C"` function pointer
as the message callback ([ffi-spike.md](ffi-spike.md)). No UMAY code calls
WebView2/WKWebView/WebKitGTK directly, so adding an adapter does not touch the
app.

## 6. Missing runtime behavior

- **WebView2 absent** (common on Windows, especially offline installs): detect
  at startup via the WebView2 availability API / registry. Fail with an
  actionable diagnostic naming the Evergreen Runtime and a download URL; do
  **not** silently download and install a bootstrapper. Bundling the fixed
  version is deferred. The `windows-arm` job must install or provide the
  runtime before the webview smoke test.
- **WebKitGTK absent** on Linux: fail with a diagnostic naming the package to
  install, resolved through the native-dependency path (`umay doctor` reports
  the resolved native deps).
- **WKWebView**: part of the OS, no fallback path needed.
- A native fallback window showing the same content is **deferred** to ADR 0020;
  v2 has no "works without any webview" UI.

## 7. Exit criteria

1. The same app — identical HTML/CSS/JS bundle and identical `window.umay`
   method schema — opens a window on Linux, macOS and Windows and renders the
   same UI.
2. A JS `umay.send` call reaches a UMAY handler and its reply is displayed in
   the DOM on each OS; an UMAY-originated event reaches JS. One CI smoke per OS
   asserts this round-trip.
3. Assets load only from `umay://app/`; a `file://` or remote URL is refused.
4. The Windows x64 and Windows ARM64 adapters both pass the round-trip smoke,
   proving the ARM64 native shell and callback ABI.
5. Missing WebView2/WebKitGTK produces the specified diagnostic, not a crash or
   a blank window.

## 8. Deferred (not in v2)

- Remote-origin apps and a cross-origin allowlist; nonce-based CSP.
- Bundled Chromium / Electron; a custom JS engine; WebView2 fixed-version
  distribution.
- Native-widget fallback or hybrid native/JS screens beyond ADR 0020.
- Mobile packaging, app lifecycle, permissions and store constraints — ADR 0021.
- Auto-generated typed JS bindings per method (manual JSON envelopes for v2).
- Remote debugging/devtools integration.
