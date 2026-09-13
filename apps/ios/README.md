# Rune iOS source

This directory contains the native SwiftUI frontend source and the narrow C
ABI declaration used to link `rune-ffi`. It is intentionally source-first:
this checkout does not download or install Xcode, an iOS SDK, simulators, or
third-party packages.

The SwiftUI view sends real command lines to the Rust session. It does not
invent terminal output. The Rust static library must be linked by the eventual
Xcode application target; a Swift package manifest alone is not an App Store
application project.

## Current evidence

- `Package.swift` is a source/package boundary.
- `RuneFFI.h` documents the C ABI layout.
- `RuneCoreBridge.swift` owns and frees Rust session handles/strings.
- `RuneTerminalView.swift` renders stdout, stderr, and non-zero exit status
  separately.
- `swiftc -parse` and `swift package dump-package` pass with the already
  available Swift toolchain; the full package build is not used as evidence.
- iOS compilation, simulator behavior, device behavior, and linking are
  currently **unverified** because no Apple build footprint is installed for
  this milestone.
