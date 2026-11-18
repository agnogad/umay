# umay-gui facade — concrete design

**Status:** design-only. Nothing in this document is implemented. No `umay-gui`
crate exists, no `Backend` value is built at run time, and the README "Not yet"
list still names the facade, fonts, menus, dialogs, asset packaging,
accessibility and webview JS↔UMAY callbacks as missing. This document turns
[ADR 0020](../adr/0020-gui-backend-strategy.md) §2–§7 into a buildable shape now
that `Box<T>`, trait objects and `dyn` dispatch exist in the language
([ADR 0018](../adr/0018-trait-object-abi.md)) — partially: capture-less handlers
work, capturing handlers and `Vec<Box<dyn T>>` do not.

## Scope and invariants

- `umay-gui` is the **public UMAY-side facade**: an object-safe `trait Backend`
  plus plain data types. Applications depend on it, never on SDL3 or a webview
  API directly.
- Backends are selected at **build time by feature**, not loaded at run time.
  Exactly one is linked. Non-GUI builds and the compiler crate are unaffected;
  the `gui` feature is reserved in [feature-flags.md](feature-flags.md) §7, and
  native GUI dependencies never enter `compiler/`.
- Everything object-safe per ADR 0020/0018: no generic methods, no `-> Self`, no
  `async fn` in the trait, associated types bound at the use site. Methods take
  and return sized, plain data; `Box<dyn Backend>` is the two-word fat pointer
  from ADR 0018 §3.

## 1. Trait surface

```umay
// design-only sketch (umay/gui/facade.um)
trait Backend {
    fn create_window(&mut self, desc: WindowDesc) -> WindowId
    fn destroy_window(&mut self, id: WindowId)
    fn poll_event(&mut self, timeout_ms: i64) -> Option<Event>
    fn present(&mut self, id: WindowId, frame: &Frame) -> Result<(), GuiError>

    fn set_menu(&mut self, id: WindowId, menu: &Menu) -> Result<(), GuiError>
    fn show_dialog(&mut self, id: WindowId, d: DialogDesc) -> DialogResult

    fn tray_set(&mut self, desc: TrayDesc) -> Result<(), GuiError>
    fn clipboard_read(&mut self) -> Option<Bytes>
    fn clipboard_write(&mut self, bytes: &Bytes) -> Result<(), GuiError>
    fn drag_drop_payload(&mut self, id: WindowId) -> Option<DropPayload>

    fn monitors(&self) -> Vec<Monitor>
    fn scale_factor(&self, id: WindowId) -> f64
    fn load_font(&mut self, spec: FontSpec) -> Result<FontId, GuiError>
    fn shutdown(&mut self)
}
```

`Result<T, E>`, `Option<T>`, `Vec<T>` and `Bytes` are the existing stdlib types
(`stdlib/option.um`, `stdlib/vec.um`, `stdlib/bytes.um`); no new container is
introduced. `Event` is an enum over close, key, mouse, resize, drop and
clipboard variants. `Frame` is a borrowed pixel buffer plus drawable size —
there is no retained widget tree, matching the immediate-mode core in
`examples/v2/gui_core.um`.

A facade call a backend cannot implement returns an explicit
`GuiError::Unsupported("menu")` rather than silently doing nothing
(ADR 0020 §5). Supported surface is recorded per backend, not guessed by the
app.

## 2. Main-thread event loop

One thread owns every window and the loop; UI state is never mutated from
another thread (also a WebView2 / AppKit / WKWebView requirement, ADR 0020 §3).

```
run(backend: &mut dyn Backend, handler: fn(Event) -> Action) -> i32
  loop:
    drain the cross-thread result queue once        // ADR 0019 / spawn_blocking
    while let Some(ev) = backend.poll_event(16):
        if handler(ev) == Action::Quit: return 0
    for each dirty window: backend.present(win, frame)
```

Long-running work goes to async tasks (ADR 0019) or blocking workers; results
are marshalled back through a queue drained once per frame. `poll_event` takes
a bounded wait so the loop stays responsive without busy-spinning. `present`
is the only place that touches the frame buffer.

## 3. Event handler ownership

- **Now:** `run` accepts a **capture-less `fn(Event) -> Action`**. It coerces to
  a plain function pointer of the correct C signature (SysV x86_64, Win64,
  AAPCS64 — ADR 0018 §4) and passes straight into SDL3's callback registration.
  No allocation, no lifetime question.
- **When it lands:** capturing handlers become
  `Box<dyn FnMut(Event) -> Action>` (ADR 0017 + ADR 0018). The compiler emits a
  **C-ABI trampoline per handler call site**, monomorphised over the closure
  type, taking `(Event, *mut u8 userdata)`. The boxed closure is the userdata:
  it is dropped with the window, and a handler borrowing a local follows the
  ADR 0017 escape rules — `move` plus owned state is the supported way to keep
  state across events. This is the design-only trampoline named by ADR 0020 §4;
  today C callbacks are only partly available and webview JS↔UMAY callbacks are
  absent (README "Not yet").
- Handler panics are contained at the trampoline boundary and reported as a
  structured diagnostic (ADR 0003); they never unwind into the native event
  loop.

## 4. Backend selection

Build features, exactly one active, no default (GUI is opt-in):

| Feature | Backend | Source |
|---------|---------|--------|
| `gui-sdl3` | SDL3 + immediate-mode UI | `examples/v2/sdl_gen.um`, `examples/v2/p5_todo_sdl.um` |
| `gui-webview` | WebView2 / WKWebView / WebKitGTK | `docs/v2/webview-decision.md`, `examples/v2/webview_gtk/` |

The facade and the app compile against the same trait either way; a `cfg`
selects which `impl Backend` is linked. Swapping backends is a rebuild, not a
runtime `dlopen`, so there is no second ABI to version. The mobile webview
([ADR 0021](../adr/0021-mobile-strategy.md)) consumes the same HTML UI as
backend 2.

### Backend 1 — SDL3

One C surface on Linux/Windows/macOS with full control of the event loop and
HiDPI. Covers window, input, clipboard and basic dialogs first; reports
`Unsupported` for full native dialogs, tray, drag-and-drop and fonts until
implemented. Drawable versus logical size and DPI scale both come through
`scale_factor` / `Frame` so widgets stay crisp (ADR 0020 §6).

### Backend 2 — webview

Reuses the mobile HTML UI and inherits the platform accessibility tree, closing
backend 1's accessibility gap (ADR 0020 §7). Native menus, dialogs and tray come
from the host toolkit behind the same trait. JS↔UMAY callbacks are the
remaining bridge and are **not implemented**; until then the webview backend can
render and post events one-way.

## 5. Explicitly not implemented

- Font loading/rendering (`load_font`, `FontSpec`) and the bundled-font fallback
  chain — ADR 0020 §6 is a decision, not code.
- Accessibility tree for backend 1 (keyboard navigation and labelled controls
  only, ADR 0020 §7); webview/native inherit it.
- Asset bundle: `umay bundle` and a bundle-relative asset path API do not exist.
- Callbacks: capturing/trampoline handlers and webview JS↔UMAY calls.
- `Box<dyn Backend>` construction end-to-end and `Vec<Box<dyn T>>`; today only
  the trait-object ABI and capture-less dispatch are in place.
- Menus, dialogs, tray and drag-drop beyond SDL3 basics; HiDPI polish.

Each item above is a facade call that returns `GuiError::Unsupported` until
landed — never a silent no-op.

## 6. Small UMAY API sketch

Illustrative, consistent with the repo's syntax (optional semicolons, `enum`,
`match`, `&mut dyn`, `Box<dyn T>`); not compiled today.

```umay
enum Action { Continue, Quit }

struct WindowDesc { title: *const u8, width: i64, height: i64 }

fn run_app(b: &mut dyn Backend) -> i32 {
    let win = b.create_window(WindowDesc {
        title: "UMAY", width: 640, height: 480,
    })
    let mut running = true
    while running {
        let ev = b.poll_event(16)
        match ev {
            Some(Event::Close(id)) => {
                b.destroy_window(id)
                running = false
            }
            Some(Event::Key(k)) => {
                if k == Key::Escape { running = false }
            }
            None => {}
            _ => {}
        }
        b.present(win, &frame_for(win))
    }
    b.shutdown()
    return 0
}
```

Backend construction (target form once `Box<dyn Backend>` is fully available):

```umay
extern "C" {
    fn umay_sdl3_backend_new() -> *mut u8
}

fn main() -> i32 {
    let b: Box<dyn Backend> = make_sdl3_backend()
    return run_app(b)
}
```

## 7. File layout proposal

```text
umay-gui/                       # new optional workspace member
  Cargo.toml                    # features: gui-sdl3 | gui-webview; default = none
  src/lib.rs                    # Rust glue: cfg backend selection, FFI shims
  src/sdl3.rs                   # #[cfg(feature = "gui-sdl3")]
  src/webview.rs                # #[cfg(feature = "gui-webview")]
  umay/
    facade.um                   # trait Backend + WindowDesc/Event/Frame/... types
    loop.um                     # run(), Action, cross-thread result queue
    handlers.um                 # fn-pointer handler; trampoline API (future)
  bindings/
    sdl3_gen.um                 # generated extern block, per examples/v2/sdl_gen.um
    webview_gen.um
```

`facade.um` and `loop.um` are pure UMAY so both backends share one definition;
generated `extern` bindings are spliced at crate root the way
`examples/v2/gui_core.um` does, because module-private items and `::` spelling
are not yet usable from the assembler.

## 8. Dependencies and open questions

- Depends on ADR 0018 (trait objects, vtable, C call signatures) and ADR 0017
  (closures) for handler ownership; on ADR 0019 for the result queue; on ADR 0020
  for backend order and ADR 0021 for the shared HTML UI.
- Open: whether the facade crate ships as stdlib-style `.um` source or as a
  Rust crate exposing UMAY bindings; the layout in §7 assumes the former with
  thin Rust glue. Confirm before implementation.
- Open: font ownership and the asset-path API need an ADR or spec section before
  `load_font` / `umay bundle` are specified beyond signatures.
