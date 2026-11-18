# ADR 0020 — Desktop GUI backend strategy

**Status:** accepted
**Phase:** v2 foundation
**Supersedes:** nothing

## Context

GUI is a v2 deliverable and needs windows, an event loop, input, menus,
dialogs, tray, clipboard and drag-and-drop on Linux, Windows and macOS. There
are three realistic routes: a low-level cross-platform C library (SDL3) with
our own immediate-mode widgets, an embedded webview (WebView2 / WKWebView /
WebKitGTK), or native toolkits (Win32 / AppKit / GTK4).

UMAY has a custom backend and an FFI story (ADR 0002, ADR 0018) but no widget
toolkit. Building three GUI stacks at once would triple the event-loop,
packaging and accessibility surface before a single app ships.

## Decision

### 1. Ordered strategy, not parallel

1. **SDL3 + a custom immediate-mode UI** for Linux, Windows and macOS.
2. **Webview backend** (WebView2 / WKWebView / WebKitGTK) for HTML UIs.
3. **Native toolkits** (Win32 / AppKit / GTK4) last, and only per platform.

SDL3 is one C surface across all three OSes, fits the custom backend and FFI
plan, and gives full control of the event loop and HiDPI handling; it ships
first. Webview reuses the same HTML UI as mobile (ADR 0021) and buys platform
accessibility cheaply. Native toolkits are the most work and the least portable
— they are the last resort for features the first two cannot provide.

Building all three at once is rejected: three event loops and three window
abstractions would multiply the bug surface, and each backend would be built
against a facade that did not yet exist. The ordered path lets backend 2 and 3
reuse the facade and the same application code.

### 2. Facade and backend trait

`umay-gui` is the public facade. It declares an object-safe `trait Backend`
(window creation, event poll, present, menus, dialogs, tray, clipboard,
drag-and-drop, monitor/HiDPI scale, font loading). `dyn Backend` is selected at
startup (ADR 0018), so the app depends only on the facade. The SDL3 backend
implements it first; the webview backend second. A build feature selects which
backend is linked.

### 3. Event loop and main thread

One main thread owns the windows and the event loop. `run(callback)` drives the
loop and invokes a closure (ADR 0017) on each event. Long-running work goes to
async tasks (ADR 0019) or `spawn_blocking`; results are marshalled back through
a queue drained once per frame. UI state is never mutated from another thread —
that is both a data-race rule and a requirement of WebView2/AppKit/WKWebView.

### 4. Closure-based handlers

`window.on_click(|ev| ...)` stores a `Box<dyn FnMut(Event)>` (ADR 0017 + ADR
0018). Capture-less handlers coerce to a plain `fn` pointer and pass straight
into SDL3's C callbacks with the correct SysV/Win64/AAPCS64 signature. Capturing
handlers are boxed and dropped with the window. A handler that borrows a local
follows ADR 0017 escape rules; `move` plus owned state is the supported way to
keep state across events.

### 5. Feature coverage by backend

The facade exposes window, menu, dialog, tray, clipboard and drag-and-drop
methods. SDL3 covers window, input, clipboard and basic dialogs on day one;
full native dialogs, tray and drag-and-drop arrive with the webview/native
backends, and a facade call on a backend that lacks it returns an explicit
"unsupported by backend" error rather than silently doing nothing. The
limitation is documented per backend.

### 6. HiDPI, fonts, theming

SDL3 reports drawable versus logical size and a DPI scale; the facade exposes
both so widgets stay crisp on scaled displays. Text uses a bundled font with a
platform fallback chain for non-Latin scripts. Theming is a token set
(colours, spacing, radius) with light/dark presets that follow the OS
preference.

### 7. Assets, accessibility, diagnostics

`umay bundle` collects images and fonts into the app package; an asset-path
API resolves them relative to the bundle, not the process CWD. Accessibility:
the webview backend inherits the platform accessibility tree and native
toolkits are fully accessible; the SDL3 immediate-mode UI provides keyboard
navigation and labelled controls but **not** a full accessibility tree — a
documented limitation of backend 1, not an excuse to skip 2. Logging goes
through `std::log`; a crash handler writes a stack trace and the structured
diagnostic payload (ADR 0003) to a user-writable path.

### 8. Exit criterion

A todo/notes app runs on the SDL3 backend, and the **same** app with an HTML UI
runs on the webview backend; both are packaged for Linux and Windows. CI
builds both and runs a headless screenshot smoke test. The mobile webview
(ADR 0021) consumes the same HTML UI.

## Consequences

- One app codebase across backends; a backend swap is a build-time choice.
- Immediate mode means no retained widget tree in the compiler or runtime.
- Backend 1 has a real accessibility gap until the webview backend lands.
- Every new facade method must be implemented on each active backend, so the
  surface is grown deliberately.

## Alternatives considered

**Native toolkits first.** Rejected: three separate implementations and the
most platform-specific code before any cross-platform app ships.

**Webview first.** Rejected on desktop: it defers the tight event-loop and
HiDPI control a game/tool needs, and SDL3 is a smaller first C surface; mobile
does go webview-first (ADR 0021) for store and packaging reasons.

**All three in parallel.** Rejected: no stable facade to build against and
three times the accessibility and packaging work at once.
