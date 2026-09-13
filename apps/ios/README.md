# Rune iOS source

This directory contains the native SwiftUI frontend source and the narrow C
ABI declaration used to link `rune-ffi`. It is intentionally source-first:
this checkout does not download or install Xcode, an iOS SDK, simulators, or
third-party packages.

The SwiftUI view sends real command lines to the Rust session. It does not
invent terminal output. The Rust static library must be linked by the eventual
Xcode application target; a Swift package manifest alone is not an App Store
application project.

The bridge also exposes a newline-delimited Rust automation script entry point
for a future Shortcuts adapter. It runs each non-empty line through the same
parser and registry, preserving combined stdout, stderr, and the last status;
native Shortcuts registration remains unverified and is not included here.

The Rust handle restores and persists only the virtual working directory and
typed command history in `~/.rune/session.state` inside the configured sandbox.
FFI command and script calls flush that state before returning, while handle
destruction performs a final best-effort flush.
Environment variables and aliases are not serialized. History persistence is
intentionally visible in the local state boundary. Interactive `export`,
`setenv`, and assignment command lines are currently replaced by a redaction
marker before history is stored; arbitrary credential-bearing commands still
require a configurable policy. The Rust core also persists bounded
`history-limit`, `font-size`, and `theme` configuration in
`~/.rune/config.state`; the bridge exposes them as key/value text and the
source-only Swift view consumes the font size and `ink`, `light`, or `ember`
palette. Cursor styling and toolbar preferences are not wired through yet.

On restore, the Rust core reads a maximum of 64 KiB from `~/.rune_profile`,
skips blank/full-line comment entries, runs only registered Rune built-ins, and
surfaces the resulting output through the FFI. Profile commands, including
bounded one-command aliases, are not added to history; the persisted working
directory is restored after the profile.

## Current evidence

- `Package.swift` is a source/package boundary.
- `RuneFFI.h` documents the C ABI layout.
- `RuneCoreBridge.swift` owns and frees Rust session handles/strings.
- `RuneTerminalView.swift` includes the `@main` SwiftUI application entry point
  and launches the real terminal view.
- The bridge asks Rust for bounded first-word completion candidates; it does
  not advertise arbitrary host executables.
- `RuneTerminalView.swift` renders stdout, stderr, and non-zero exit status
  separately, applies the Rust-backed first-word suggestions, and provides the
  native focused command bar/history controls. It consumes Rune's clear-screen
  control sequence as a display action instead of showing escape bytes.
- `swiftc -parse` and `swift package dump-package` pass with the already
  available Swift toolchain; the full package build is not used as evidence.
- iOS compilation, simulator behavior, device behavior, and linking are
  currently **unverified** because no Apple build footprint is installed for
  this milestone.
