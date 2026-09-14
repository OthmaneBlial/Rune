# Contributing to Rune

Thanks for helping make a bounded, inspectable terminal for Apple platforms.
Contributions are welcome in Rust core behavior, VFS policy, FFI contracts,
source-only Swift, compatibility evidence, tests, and documentation.

## Before you start

Read docs/architecture.md, docs/development.md, and docs/compatibility.md. The
base/a-shell/ checkout is a behavioral reference only; it is ignored and must
never be staged or copied into Rune.

## Local workflow

~~~bash
git clone https://github.com/OthmaneBlial/Rune.git
cd Rune
cargo build --workspace
./scripts/ci.sh
~~~

The project intentionally does not download Xcode, iOS SDKs, simulators, or
other Apple build footprints. If the existing machine has Swift tools,
scripts/ci.sh performs source-only package/header/type checks. Those checks are
not an iOS runtime build.

## Pull requests

- Keep changes focused and explain the user-visible behavior.
- Add normal and error-path tests for new Rust behavior.
- Preserve VFS confinement, output/input bounds, explicit capability providers,
  and safe default behavior.
- Update the relevant docs, compatibility evidence, changelog, or roadmap.
- Use conservative wording: distinguish working locally, source-only,
  experimental, pending, and unverified.
- Run scripts/ci.sh and include the result in the pull request.
- Never include passwords, private keys, tokens, personal host inventories,
  real credentials, or private captures.

## Commit style

Use a short imperative subject with a focused scope, for example:

~~~text
feat(core): add bounded command behavior
docs: clarify Apple runtime gates
fix(fs): reject escaped canonical paths
~~~

## Compatibility observations

Reference observations must identify a-Shell, include the matching scenario
id, and contain captured stdout, stderr, and status. Do not upgrade the matrix
from pending based on source inspection or a self-comparison.
