# Rune

Rune is a Rust-native Unix-like terminal environment for iPhone and iPad, with
a first-class native Apple frontend planned around Swift and SwiftUI. It is an
independent implementation, designed from first principles for iOS sandbox
constraints, reliable command execution, and portable core behavior.

The local `base/a-shell/` checkout is a behavioral and product reference. It is
excluded from Git and no a-Shell source is part of Rune.

## Development Progress

**Overall progress: 24%**

This is an intentionally conservative engineering estimate. The repository
foundation and first Rust shell slice are locally verified; there is not yet a
working iOS application or a feature-parity claim.

| Area | Progress |
|---|---:|
| Rust workspace | 70% |
| Shell tokenizer/parser | 60% |
| Command runtime | 71% |
| Sandboxed filesystem | 55% |
| Sessions/history | 38% |
| WASM | 23% |
| Native iOS UI | 9% |
| Swift/Rust bridge | 8% |
| Package manager | 25% |
| Compatibility evidence | 2% |

## Current status

The first Rust vertical slice is implemented and locally verified. It executes
the supported built-in commands `pwd`, `cd`, `ls`, `cat`, `echo`, `mkdir`,
`touch`, `rm`, `cp`, `mv`, `env`, `export`, `unset`, `printenv`, `setenv`,
`alias`, `unalias`, `find`, `sed`,
`true`, `false`, `head`, `tail`, `grep`, `sort`, `uniq`, `wc`, `wasm`, `pkg`,
`bookmark`, `showmarks`, `jump`, `renamemark`, `deletemark`, `clear`, `help`,
and `history` against a bounded filesystem, including basic `*`/`?` pathname
expansion with quote and hidden-file rules,
with quotes, variables, leading `NAME=value` assignments, pipes, redirections,
sequencing, `&&`/`||` short-circuiting, separate stdout/stderr, and exit status.
Assignments are expanded left-to-right, remain session-local, and can also be
issued without a command.
The text pipeline also supports a bounded literal `sed` substitution surface:
`s///` with optional `g`/`p` flags and `-n`; regular expressions and addresses
are not implemented yet.
A synchronous command response is capped at 1 MiB per output channel; a
truncation marker is emitted rather than allowing unbounded terminal output.
Environment changes are not serialized. A bounded `~/.rune_profile` is loaded
on restore; its supported Rust built-ins can update the session environment and
define aliases, with output surfaced to the CLI/native boundary without
polluting history. Alias expansion is bounded and currently accepts one
command per alias value; compound alias values are rejected explicitly. The
native source UI now
receives the Rust command registry and offers first-word command suggestions.
It also has a focused command bar, keyboard-aware history controls, an
ink/cyan/ember console palette, and accessible completion controls.
Interactive `export`, `setenv`, and assignment lines are replaced by a
redaction marker in history before persistence; this is an initial defense, not
a complete secret management policy.
The iOS app is represented by
source-only SwiftUI and FFI boundaries, but its Apple compilation, linking,
and runtime gates remain unverified. Bounded current-directory/history
persistence now exists in Rust; configuration/redaction and broader session
recovery remain planned. Package transport and non-WASM language runtimes
remain planned work.

The Rust package boundary now validates a bounded, versioned JSON manifest and
checks declared file bytes with SHA-256. There is deliberately no network
transport, registry, search, or update flow yet, so package-manager progress
remains early.

The `wasm MODULE [arg ...]` built-in loads a module through the bounded virtual
filesystem and executes WASI preview1 `_start` in Rust. It exposes only
stdin/stdout/stderr, arguments, and the session environment; it does not
preopen a host directory. Module bytes, interpreter fuel, linear memory,
tables, and captured output are bounded. This is an initial WASM execution
slice, not a language runtime or package manager.

The local package flow supports `pkg info MANIFEST`, `pkg verify MANIFEST`,
`pkg install MANIFEST`, `pkg list`, and `pkg remove NAME [VERSION]`. Install
copies only SHA-256-verified files into `~/.rune/packages`; declared `.wasm`
commands can then run through the bounded WASI runtime. Network transport,
registry search, and update remain unsupported at this stage.

The portable core also supports session-local virtual directory bookmarks with
`bookmark`, `showmarks`, `jump`, `cd ~NAME`, `renamemark`, and `deletemark`.
They persist with the session state and remain confined to the configured VFS;
external folders and security-scoped bookmark resolution are still Apple-side
work.

The FFI and Swift source boundary also exposes a newline-delimited automation
script method for a future Shortcuts adapter. It is Rust-executed and locally
tested, but native Shortcuts registration remains unverified.

Runtime providers use a small Rust-owned request/output contract. WASM is the
first provider; Python, JavaScript, and Lua are named extension points only and
remain unavailable until their execution and App Store boundaries are designed
and tested.

No a-Shell compatibility area is marked `supported` without behavior and test
evidence. See [`compat/a-shell-compatibility.json`](compat/a-shell-compatibility.json).

## Architecture

```text
Swift / SwiftUI app (source-only; Apple link unverified)
            │ narrow C ABI via rune-ffi
            ▼
      rune-core  ─── command registry and session orchestration
        │   │
        │   └──── rune-fs   bounded filesystem abstraction
        ├──────── rune-wasm WASI preview1 interpreter boundary
        ├──────── rune-package manifest and integrity boundary
        ├──────── rune-shell tokenizer, parser, execution plan
        └──────── rune-ffi   owned C ABI handles and output buffers
```

The portable crates own shell semantics and platform-independent policy. An
Apple adapter will supply sandbox paths, document-picker access, and
security-scoped bookmark behavior without moving core command logic into
Swift.

## Local validation

There is deliberately no GitHub Actions workflow. Run the local quality gate:

```bash
./scripts/ci.sh
```

When `swiftc` is already available, the gate also parses the native Swift
sources. This is syntax evidence only; it is not simulator, device, or App
Store evidence.

The reference checkout is intentionally ignored and can be checked with:

```bash
git check-ignore -v base/a-shell
```

## Roadmap

### Phase 1 — Foundation

- [x] Repository and Rust workspace
- [x] Local-only quality workflow
- [x] First command execution slice
- [ ] SwiftUI iOS application shell
- [ ] Stable Rust/Swift bridge

### Phase 2 — Core shell

- [x] Tokenizer and parser
- [x] Environment and path expansion
- [x] Bounded virtual filesystem
- [x] Built-in file commands
- [x] Bounded session/history persistence and startup profile
- [x] Pipes and redirections
- [x] Basic bounded pathname expansion
- [x] Leading environment assignments
- [x] Bounded session-local command aliases
- [x] `&&` and `||` conditional chaining
- [x] Bounded recursive `find` traversal
- [x] Bounded literal `sed` substitutions
- [x] Bounded terminal output channels
- [x] Session-local virtual directory bookmarks

### Phase 3 — Developer environment

- [x] Bounded WASI preview1 runtime boundary and resource limits
- [x] Bounded package metadata, integrity, and local WASM installation
- [x] Portable runtime request/output contract
- [ ] Network registry, search, and update policy
- [ ] Python, JavaScript, and Lua runtime evaluation
- [ ] Completion and help system (initial first-word suggestions exist)

### Phase 4 — Apple integration

- [ ] Fast native terminal rendering
- [ ] External folders and security-scoped bookmarks
- [ ] Multiple sessions and iPad multi-window behavior
- [ ] Apple Shortcuts actions
- [ ] Accessibility and VoiceOver validation

## Non-goals for the current milestone

- claiming a-Shell parity
- copying a-Shell implementation or UI
- executing arbitrary host processes from the shell
- pretending an iOS app exists before it is built and tested
