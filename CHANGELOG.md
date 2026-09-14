# Changelog

All notable public changes to Rune are documented here. The project is
pre-1.0; entries describe verified repository behavior and do not imply an
iOS runtime release.

## [0.1.0-alpha] - 2026-09-14

### Highlights

- Rust-native Unix-like shell core with bounded parsing, execution, output, and
  cancellation semantics.
- Confined virtual filesystem with persistence, bookmarks, history, terminal
  state, and named sessions.
- Built-in filesystem, text, archive, checksum, runtime, WASM, package, network,
  and external-capability command surfaces.
- Narrow C FFI and source-only SwiftUI/UIKit boundary for the eventual iOS and
  iPadOS application.
- Local compatibility matrix and differential scenario runner that keep direct
  a-Shell evidence explicitly pending.

### Validation

- ./scripts/ci.sh passes on the tagged commit.
- Rust workspace formatting, Clippy, tests, and build pass locally.
- Swift package/header checks and host swiftc -typecheck pass when the
  existing Swift tools are available.
- The local gate compiles and runs a C consumer against the public FFI header
  and host library, including owned-result release checks.

### Known limitations

- No Xcode, iOS SDK, simulator, or device runtime is included or installed by
  this project workflow.
- The Swift package is a source boundary, not an App Store application target.
- Apple provider success, URL routing, networking, media, clipboard, Contacts,
  signing, and linking remain runtime gates.
- Direct a-Shell comparison remains pending; the local base/a-shell/ checkout
  is reference-only and ignored.
