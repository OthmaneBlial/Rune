# Changelog

All notable public changes to Rune are documented here. The project is
pre-1.0; entries describe verified repository behavior and do not imply an
iOS runtime release.

## [0.1.0-alpha.8] - 2026-09-14

### Added

- Reproducible macOS arm64 CLI packaging with a published SHA-256 checksum.
- GitHub contribution templates, Dependabot configuration, and security
  reporting links.

## [0.1.0-alpha.7] - 2026-09-14

### Added

- A bounded Rust-native `seq` utility for ascending and descending integer
  sequences, with explicit zero-increment and output-size errors.

## [0.1.0-alpha.6] - 2026-09-14

### Added

- Bounded ANSI insert mode (`CSI 4h` / `CSI 4l`) for cursor-positioned text
  insertion in the Rust terminal grid.

## [0.1.0-alpha.5] - 2026-09-14

### Added

- Bounded DECSCUSR blink modes are now propagated through the public cursor FFI
  contract and rendered by the source-only SwiftUI timeline surface.

## [0.1.0-alpha.4] - 2026-09-14

### Added

- Bounded DECSCUSR cursor-shape requests (`CSI Ps q`) from the Rust terminal
  grid through the public C FFI to the source-only SwiftUI caret renderer.

## [0.1.0-alpha.3] - 2026-09-14

### Added

- Bounded alternate terminal screen support through `CSI ?1049h` / `CSI
  ?1049l`, including primary-screen restoration after viewport resizing.

## [0.1.0-alpha.2] - 2026-09-14

### Added

- Bounded terminal last-character repetition through `CSI b`, with regression
  coverage for cursor advancement and grid limits.

## [0.1.0-alpha.1] - 2026-09-14

### Added

- Rust terminal cursor visibility state for bounded `CSI ?25l` / `CSI ?25h`
  controls, exposed through the C ABI and consumed by the source-only SwiftUI
  terminal surface.
- Public C FFI smoke coverage for cursor visibility transitions.
- Bounded `printf` octal and hexadecimal escapes for real ANSI-oriented
  terminal workflows.

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
- Rust-planned scripts can be launched from the CLI with repeated
  `--script-arg` values that populate bounded positional parameters.
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
