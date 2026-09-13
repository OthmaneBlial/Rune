# Rune development

## Prerequisites

- Rust toolchain with `cargo`, `rustfmt`, and Clippy
- Xcode for future iOS validation

## Local checks

Run `./scripts/ci.sh` from the repository root. It runs formatting, Clippy,
workspace tests, and a workspace build. The project intentionally does not use
GitHub Actions or cloud CI.

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

## Evidence discipline

Local tests prove local behavior only. They do not prove App Store review,
device performance, a-Shell compatibility, or package security. Those claims
need their own evidence and must remain explicitly unverified until tested.
