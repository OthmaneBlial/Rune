# Rune roadmap

Rune is an independent Rust-native terminal project for iPhone and iPad. This
roadmap separates portable work from Apple runtime gates so source-level
progress is not mistaken for a shipped application.

## Now — alpha source preview

- Keep the Rust CLI, VFS, shell planner, persistence, runtimes, archives, and
  FFI contracts bounded and regression-tested.
- Keep the SwiftUI/UIKit layer source-first without downloading Xcode, SDKs, or
  simulators in the disk-constrained workspace.
- Maintain the a-Shell compatibility matrix as pending until separate Apple
  observations exist.
- Improve the public documentation, demo, and release hygiene around verified
  behavior.

## Next — Apple validation

- Build and link the native app with a real iOS toolchain.
- Validate command execution, terminal rendering, keyboard input, accessibility,
  settings, folder access, cancellation, tabs, and window routing on simulator
  and hardware.
- Exercise the URL, network, clipboard, media, and folder providers against
  Apple runtime APIs and record their failure paths.

## Later — platform depth

- Add direct scenario-based a-Shell observations and documented normalization
  rules where behavior is intentionally compatible.
- Decide which toolchain providers are safe and supportable on iOS.
- Expand terminal control coverage, iPad multi-window behavior, and installable
  release artifacts.
- Revisit package distribution only when a genuine consumer-facing artifact can
  be built and smoke-tested on the target platform.

## Explicitly not claimed yet

- A working App Store or TestFlight build.
- iOS simulator or device compatibility.
- Full a-Shell or POSIX parity.
- A bundled C/C++ compiler, TeX engine, host shell, or unrestricted process
  launcher.
- Network, TLS, ATS, Contacts, or external-application success without a
  validated Apple provider and runtime evidence.
