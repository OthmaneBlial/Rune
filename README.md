# Rune

**A Rust-native Unix-like terminal core for iPhone and iPad.**

Rune brings a bounded, local-first shell workflow to Apple platforms: Rust owns
command execution, the confined virtual filesystem, persistence, runtimes, and
the FFI boundary; SwiftUI provides the native presentation layer. It is an
independent implementation, not a web terminal and not a feature-parity claim
for a-Shell.

## Development Progress

**Overall progress: 55%**

This is an engineering estimate of the product path toward a validated iPhone
and iPad terminal, not a percentage of lines of code. Portable Rust behavior is
ahead of the Apple runtime because the latter is intentionally source-only in
this disk-constrained workspace.

| Area | Progress |
| --- | ---: |
| Rust core and session model | 68% |
| Shell grammar and execution planning | 59% |
| Built-in command surface | 54% |
| Confined filesystem and security policy | 62% |
| Persistence and command history | 55% |
| Terminal state and ANSI handling | 62% |
| Rust/Swift FFI contracts | 64% |
| SwiftUI/UIKit source boundary | 38% |
| WASM, scripting, and toolchain runtimes | 28% |
| Direct a-Shell comparison | 5% |
| Apple runtime, packaging, and release validation | 10% |

<p align="center">
  <a href="https://othmaneblial.github.io/Rune/"><img src="https://img.shields.io/badge/project_site-Rune-e85d2a?style=flat-square" alt="Rune project site"></a>
  <a href="https://github.com/OthmaneBlial/Rune/releases"><img src="https://img.shields.io/github/v/release/OthmaneBlial/Rune?include_prereleases&style=flat-square&color=0ea5a8" alt="Latest Rune release"></a>
  <a href="https://github.com/OthmaneBlial/Rune/blob/main/LICENSE"><img src="https://img.shields.io/github/license/OthmaneBlial/Rune?style=flat-square&color=f2b84b" alt="MIT license"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/core-Rust_1.78%2B-orange?style=flat-square&logo=rust" alt="Rust 1.78 or newer"></a>
</p>

<p align="center">
  <a href="https://othmaneblial.github.io/Rune/">Website</a> ·
  <a href="https://othmaneblial.github.io/Rune/media/rune-cli-demo.mp4">Watch the real CLI demo</a> ·
  <a href="https://othmaneblial.github.io/Rune/docs.html">Documentation</a> ·
  <a href="docs/performance.md">Performance</a> ·
  <a href="ROADMAP.md">Roadmap</a> ·
  <a href="https://github.com/OthmaneBlial/Rune/issues">Issues</a>
</p>

<p align="center">
  <a href="https://othmaneblial.github.io/Rune/media/rune-cli-demo.mp4">
    <img src="https://raw.githubusercontent.com/OthmaneBlial/Rune/main/site/media/rune-cli-demo-poster.png" alt="Rune CLI demo showing a confined project tree, a Rust source search, persisted working directory, and frequency-ranked directory jumping" width="900">
  </a>
</p>

> The preview above is a designed presentation built from real Rust CLI output
> captured against a temporary confined filesystem. It demonstrates the portable core; an iOS
> build, simulator, and device runtime remain unverified in this
> disk-constrained workspace.

## Why Rune exists

Mobile terminals have to balance familiar Unix workflows with strict platform
boundaries. A useful iPhone or iPad terminal cannot quietly depend on a host
shell, arbitrary filesystem paths, unbounded output, or ambient network and
process access.

Rune makes those boundaries explicit. The core is useful on its own, while the
native layer can add Apple capabilities through narrow, typed providers. That
keeps the shell testable on a developer machine and keeps platform behavior
honest when an Apple runtime is not available.

## What you get

### A real Rust shell workflow

- Rust tokenization and planning for quotes, variables, pipes, redirections,
  command substitution, sequencing, &&/||, scripts, loops, functions, and
  bounded positional arguments.
- A growing Unix-like built-in surface including filesystem commands, text
  filters, archives, checksums, awk, sed, grep, sort, wc, tar, gzip,
  wasm, python3, lua, and jsc.
- Session-local history, bookmarks, frequency-ranked z navigation, aliases,
  configuration, terminal state, and named-session persistence.

### A confined filesystem and explicit capabilities

- Virtual ~, ~/Library, and ~/tmp mounts backed by approved roots.
- Canonical-path checks, symlink containment, bounded reads/writes/traversals,
  and no host-shell fallback.
- Network, clipboard, external URL/file opening, media preview, folder import,
  and toolchain execution exposed only through explicit providers. The default
  CLI providers are disabled.

### A native Apple boundary

- rune-ffi exposes opaque session handles, owned buffers, command events,
  cancellation, terminal snapshots, folder actions, and host-provider
  callbacks.
- apps/ios contains the SwiftUI/UIKit source boundary, settings surface,
  keyboard actions, App Intents declarations, external-folder access, and
  terminal presentation.
- The Swift package and source-only swiftc -typecheck checks are available
  without installing Xcode, an iOS SDK, or a simulator.

## Demo

The 54-second demo is built from actual rune-cli runs and shows:

1. Creating a project inside a temporary confined root.
2. Rendering the bounded virtual tree and finding Rust files.
3. Running a real text pipeline through grep.
4. Restoring state in a new process and using z directory navigation.
5. Inspecting the Rust-owned command/configuration surface.
6. The boundary between verified portable code and the still-unverified Apple
   runtime.

<p align="center">
  <a href="https://othmaneblial.github.io/Rune/media/rune-cli-demo.mp4"><strong>▶ Watch the full MP4 demo</strong></a>
</p>

This is intentionally a CLI proof, not a fabricated iPhone recording or a
claim that the source-only Swift layer has been launched. The
repository has no existing iOS capture and this workspace does not contain the
Apple build footprint needed to produce one.

## Current status

| Area | Evidence-backed status |
| --- | --- |
| Rust workspace and CLI | **Working locally** — formatted, linted, tested, and built |
| Shell, VFS, persistence, archives, text filters, and bounded runtimes | **Working locally** — Rust regression coverage exists |
| C FFI and host capability contracts | **Working locally** — callback and ownership boundaries are tested |
| SwiftUI/UIKit source boundary | **Source-only** — package manifest, C header target, and host swiftc typecheck pass |
| iOS application build, linking, simulator, and device behavior | **Unverified** — no Xcode/SDK/simulator installed |
| Direct a-Shell behavior comparison | **Pending** — the checkout is reference-only and no Apple harness is available |
| Release | **v0.1.0-alpha.6** — source preview, not a stable iOS application |

The compatibility matrix in
[compat/a-Shell-compatibility.json](compat/a-shell-compatibility.json)
keeps bounded implementation status separate from direct reference evidence.
partial means a tested Rune subset; it does not mean parity.

## Quick start

### Run the portable CLI

Requirements: Rust 1.78 or newer. No Xcode or Apple SDK is needed for this
path.

~~~bash
git clone https://github.com/OthmaneBlial/Rune.git
cd Rune
cargo run -p rune-cli -- --root /tmp/rune-root \
  -c "mkdir -p project/src; echo 'fn main() {}' > project/src/main.rs; tree project"
~~~

The root passed to rune-cli is the only filesystem exposed to that session.
For a clean repeatable example and a persistence round trip:

~~~bash
./scripts/demo.sh
~~~

The CLI also exposes its release identity without creating a session:

~~~bash
target/debug/rune-cli --version
# rune-cli 0.1.0
~~~

To run a Rust-planned script stored inside the confined root:

~~~bash
target/debug/rune-cli --root /tmp/rune-root --script project/profile.rune \
  --script-arg development
~~~

`--script` resolves a virtual path through Rune's filesystem and executes it
with the same bounded `source` planner used by the native bridge. It never
launches the host shell. Repeat `--script-arg VALUE` to pass bounded positional
arguments to the script (`$0`, `$1`, and so on); the CLI accepts at most 64
values, with a 16 KiB limit per value and a 64 KiB total generated command
limit.

### Check the source-only iOS boundary

The repository deliberately does not install Apple tooling. If the existing
machine already has the host Swift tools, the local CI performs:

~~~bash
cd apps/ios
swift package dump-package
swift build --target RuneFFIHeaders
~~~

These commands validate package and C-header structure only. They do not build
or launch an iOS application.

## Build and validate from source

Run the project gate from the repository root:

~~~bash
./scripts/ci.sh
~~~

The local gate validates the compatibility documents, shell script syntax,
Rust formatting/Clippy/tests/build, the Swift package boundary, and a
source-only Swift typecheck when swiftc is already present. It does not
contact a live a-Shell instance, start an iOS simulator, or claim Apple
runtime behavior.

It also compiles a small C consumer against the public Rust FFI header and
host library. This validates the ownership and execution boundary without
installing Xcode or an Apple SDK.

Useful focused commands:

~~~bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace
python3 scripts/validate_compatibility.py
python3 scripts/compatibility_runner.py --validate-only
~~~

## How it works

~~~text
SwiftUI / UIKit source-only frontend
              │
              ▼
       RuneCoreBridge.swift
              │ narrow C ABI
              ▼
          rune-ffi
              │
              ▼
          rune-core ───── explicit host providers
          │   │   │       (network / open / clipboard / tools)
          │   │   └─────── disabled by default in the CLI
          │   ├────────── rune-fs
          │   ├────────── rune-shell
          │   ├────────── rune-runtime
          │   ├────────── rune-wasm
          │   └────────── rune-package
          ▼
   bounded command result:
   stdout + stderr + status + events
~~~

Rust owns the command registry, session state, path policy, limits, output
semantics, and persistence. Swift owns presentation and copies data across the
ABI; it does not maintain a second shell or filesystem implementation.

## Security and privacy boundaries

- No command is delegated to a host shell, and the virtual filesystem rejects
  canonical paths outside approved roots.
- Output, command input, script size, archive traversal, runtime execution, and
  persisted state are bounded before untrusted growth can reach the UI.
- Network and external-application actions require an injected provider;
  disabled providers fail visibly instead of silently using ambient access.
- History redaction is enabled by default for environment assignments, network
  requests, and direct phone/SMS commands. Environment persistence is opt-in and
  is not a secret store.
- Development diagnostics contain safe execution metadata only. Do not place
  passwords, private keys, tokens, personal host inventories, or real captures
  in issues, fixtures, or pull requests.
- base/a-shell/ is a local behavioral reference, ignored by Git, and never
  part of the Rune source tree or release.

Read [docs/architecture.md](docs/architecture.md) for the capability model,
[docs/apple-validation.md](docs/apple-validation.md) for the future runtime
validation hand-off, and [SECURITY.md](SECURITY.md) for reporting guidance.

## Roadmap

The short public roadmap is in [ROADMAP.md](ROADMAP.md). The next meaningful
gates are:

- validate the native SwiftUI app with a real Apple build, simulator, and
  device;
- complete direct, scenario-based a-Shell observations without changing the
  conservative compatibility policy;
- connect reviewed Apple providers for network, open, clipboard, and toolchain
  capabilities;
- expand terminal rendering and iPad multi-window behavior with runtime
  evidence;
- produce a signed, installable iOS artifact only after the target environment
  is available.

## Contributing

Start with [CONTRIBUTING.md](CONTRIBUTING.md), then run ./scripts/ci.sh
before opening a pull request. Focused Rust tests, VFS/security reviews,
compatibility observations, Swift source improvements, and documentation are
all useful contributions. Please keep claims scoped to the evidence in the
repository and do not add GitHub Actions or toolchain downloads to work around
the local source-only policy.

## License

Rune is released under the [MIT License](LICENSE).
