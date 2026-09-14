# Apple validation runbook

This document is the hand-off for the first runtime validation of Rune on
iPhone and iPad. It deliberately does not install Apple tooling. The current
workspace proves Rust behavior, the public C ABI, the Swift package manifest,
and source-only Swift typechecking; it does not prove an iOS build or runtime.

## Prerequisites

Use a machine with an Apple developer toolchain already installed:

```bash
xcodebuild -version
xcrun simctl list devices available
swift --version
rustup show active-toolchain
```

The selected Xcode must provide an iOS SDK and a simulator runtime. A signed
physical-device check additionally needs a development team and a provisioning
profile. Do not add credentials, signing certificates, or provisioning files to
the repository.

## Source and Rust checks

From the repository root, run the existing local gate first:

```bash
./scripts/ci.sh
```

Build the Rust FFI library for the exact Apple target selected by the Xcode
project, then link that generated library through the app target. The
repository must retain the same public header at
`apps/ios/Sources/RuneFFIHeaders/include/RuneFFI.h`; do not substitute a host
library for an iOS artifact.

## Runtime checklist

Record each result as `passed`, `failed`, or `unverified`, with the device or
simulator model, OS version, build identifier, and captured logs:

1. Launch the app and create a default Documents/Library/tmp session.
2. Run `pwd`, `ls`, `mkdir`, `touch`, `cat`, a pipeline, and a failing command.
3. Confirm stdout, stderr, exit status, cursor updates, ANSI output, and
   cancellation are rendered without duplicate events.
4. Exercise keyboard submission, hardware-keyboard history, completion, copy,
   paste, clear, font/theme settings, and VoiceOver labels.
5. Open, rename, restore, and remove an approved external folder bookmark;
   verify that paths outside the selected root are rejected.
6. Create multiple tabs and a second window; verify independent session IDs,
   persistence, and close/restore behavior.
7. Exercise the network, open, media, clipboard, and toolchain providers only
   with explicit host callbacks, and record denied-provider failures.
8. Run the declared App Intent actions and record registration, permissions,
   returned output, and failure behavior.

## Release evidence

Do not create a stable release or attach a binary until the runtime checklist
passes. A release candidate should include the exact target, SDK/OS versions,
archive/export result, signing status, install result, smoke-test transcript,
and checksums. If any Apple item is unavailable, keep it explicitly
`unverified` and retain the alpha source-preview status.
