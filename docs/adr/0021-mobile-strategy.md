# ADR 0021 — Mobile strategy

**Status:** accepted
**Phase:** v2 foundation
**Supersedes:** nothing

## Context

v2 includes mobile. A UMAY app must run on iOS and Android with one business
logic core, be signable for simulator and device, and produce a store-uploadable
package. Unlike desktop, each platform controls the UI toolkit, the build
system, the code-signing chain and the app lifecycle, and both require specific
target triples, load commands, PIE, libc choice and privacy metadata.

This ADR records the target/b/package surface, not the UI design; the HTML UI
is shared with the desktop webview backend (ADR 0020) and the async I/O with
ADR 0019.

## Decision

### 1. Webview-first; native UI later

iOS uses **WKWebView**, Android uses **WebView**. Native UI bindings (UIKit /
Android Views) are optional and come later, only for features the webview
cannot provide. Rationale: one HTML UI shared with ADR 0020 and ADR 0019, one
business-logic core compiled to both targets, no two native widget sets.

### 2. iOS cross-compilation and signing

- Target `aarch64-apple-ios`; simulator target `aarch64-apple-ios-sim` on a
  macOS runner.
- Emit Mach-O with the correct platform/version load commands
  (`LC_BUILD_VERSION` for minimum OS and SDK, `LC_VERSION_MIN_IPHONEOS` where
  required for older toolchains).
- Link with the Xcode toolchain. Code signing requires a development or
  distribution certificate, a provisioning/profile entitlement set and an
  explicit bundle identifier; unsigned binaries do not run on device.
- SPM/Xcode integration: generate an Xcode project or Swift package that embeds
  the UMAY static library, exposes a small C header, and hosts WKWebView. Build
  and test on simulator and device.

### 3. Android cross-compilation

- Targets `aarch64-linux-android` and `x86_64-linux-android` (emulator).
- Output is an ELF **PIE** (Android requires position-independent executables),
  linked against **Bionic**, not glibc, using the NDK sysroot and Clang cross
  toolchain. Do not link the host C library.
- Gradle plugin packages the UMAY library into an **AAR**; an **APK** is built
  for testing and an **AAB** for store upload. The manifest declares the
  package, version and permissions (ADR 0018 FFI/bindgen supplies the `extern
  fn` declarations for the bridge).

### 4. JNI bridge

A thin JNI layer exposes UMAY entry points as `extern fn` functions that
Java/Kotlin calls; the facade calls back into UMAY business logic. JNI types
(`jobject`, `jstring`, `jbyteArray`) are declared through the FFI/bindgen path
of ADR 0018, with the correct Win64/SysV/AAPCS64-style call lowering — on
Android that is AAPCS64 for arm64 and SysV for x86_64. The bridge is small and
generated, not hand-maintained per function.

### 5. App lifecycle

Foreground/background, suspend/resume and low-memory callbacks are delivered
to the UMAY side through the bridge. On background, async tasks (ADR 0019)
pause and open sockets are released; on resume they restart. On low memory,
caches are dropped. iOS and Android may kill a backgrounded app at any time, so
state must be persisted on suspend, not assumed to survive.

### 6. Storage, assets, network, native services

Sandboxed app directories hold writable files; assets ship in the bundle and
resolve through the ADR 0020 asset API; network uses the async wrappers
(ADR 0019). Secure data goes through Keychain (iOS) / Keystore (Android) via a
native bridge. Notifications, sensors, camera and location are exposed as thin
native shims calling UMAY through the same bridge — they are not part of the
core runtime.

### 7. CI hardware plan

- **iOS**: a macOS runner builds and runs the simulator target; no Apple
  Silicon runner is available to this private repository (ADR 0015), so a
  real-device test needs a self-hosted Mac or an external device farm.
- **Android**: the emulator needs KVM/hardware acceleration — a larger or
  self-hosted Linux runner; real-device tests need physical hardware.

This **exceeds the current private-repo hosted CI** (`.github/workflows/ci.yml`
runs Linux, macOS-intel, Windows and Windows-ARM only). Mobile CI therefore
requires added self-hosted runners or an external device farm; the required
runner set is documented explicitly rather than pretending the current matrix
covers it.

### 8. Store requirements

Privacy manifest and permission-description strings (`NSCameraUsageDescription`,
etc.) on iOS; 64-bit binaries, target API level and an AAB on Android. Signing
credentials are supplied by the release pipeline, never committed. The
store-uploadable outputs are an **IPA** and an **AAB**.

### 9. Exit criterion

One UMAY business-logic package runs on iOS and Android; simulator and device
builds are signed; the outputs are store-uploadable IPA and AAB.

## Consequences

- One business logic and one HTML UI across desktop webview and both mobile
  platforms.
- A device/simulator CI tier is a new infrastructure dependency beyond today's
  private-repo runners.
- Store metadata (permissions, privacy manifest, signing) is part of the build,
  not an afterthought.
- Native UI is deferred; the webview defines the mobile UI ceiling in v2.

## Alternatives considered

**Native UI first (UIKit / Android Views).** Rejected: two widget sets and no
sharing with the desktop webview backend.

**Compile to WebAssembly in a webview.** Rejected: the custom native backends
and ownership model are the point of the project; mobile targets native code
with a webview for UI only.

**Rely on the current hosted CI alone.** Rejected: it has no macOS simulator
runner with signing and no accelerated Android emulator; a hardware plan is
required.
