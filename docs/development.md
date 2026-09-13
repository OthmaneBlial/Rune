# Rune development

## Prerequisites

- Rust toolchain with `cargo`, `rustfmt`, and Clippy
- Existing Apple/Swift tooling is optional for source inspection; do not install
  Xcode, simulators, SDKs, or additional dependencies just for this phase.

The portable WASM slice is validated through Rust tests and does not require
Xcode, a simulator, an Apple SDK, or a third-party Apple dependency.
Package metadata and local WASM installation tests are also local-only; no
registry or download is needed to validate the manifest, digest, and bounded
package-tree boundaries.
The package search test is local-only as well and searches installed
manifests without network access.
Configuration tests are local-only as well and verify bounded history, font
size, and theme settings without requiring an Apple runtime.
Completion tests also use only the confined VFS and do not require an Apple
runtime.
The source-only Apple check also parses the external-folder bookmark layer;
that syntax check does not prove security-scoped access, entitlements, or Files
picker behavior on a device.
It also parses the source-only App Intent declarations; the AppIntents module,
registration, entitlements, and Shortcuts runtime remain unverified here.

## Local checks

Run `./scripts/ci.sh` from the repository root. It runs formatting, Clippy,
workspace unit and integration tests, and a workspace build. The project
intentionally does not use GitHub Actions or cloud CI.

Run `./scripts/demo.sh` for a real two-process CLI workflow covering file
creation, recursive copy, configuration persistence, restore, metadata, usage,
and history. It uses a temporary root and removes only that root on exit.

## Commit boundaries

Keep commits small enough to describe one coherent behavior. Before a push:

1. inspect `git diff` and `git status`
2. verify `base/` is absent from the staged diff
3. check for secrets and local credentials
4. run the relevant local checks
5. update documentation and the progress estimate when scope changes
6. commit and push directly to `main`

The reference implementation is never staged. `git check-ignore -v
base/a-shell` should report the repository's `/base/` rule.

Apple work is developed as source and narrow interfaces until an already
available Apple toolchain can validate it. A local Rust green check does not
prove Swift compilation, simulator behavior, device behavior, or App Store
readiness.

## Evidence discipline

Local tests prove local behavior only. They do not prove App Store review,
device performance, a-Shell compatibility, or package security. Those claims
need their own evidence and must remain explicitly unverified until tested.
