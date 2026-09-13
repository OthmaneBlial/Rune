# Rune

Rune is a Rust-native Unix-like terminal environment for iPhone and iPad, with
a first-class native Apple frontend planned around Swift and SwiftUI. It is an
independent implementation, designed from first principles for iOS sandbox
constraints, reliable command execution, and portable core behavior.

The local `base/a-shell/` checkout is a behavioral and product reference. It is
excluded from Git and no a-Shell source is part of Rune.

## Development Progress

**Overall progress: 44%**

This is an intentionally conservative engineering estimate. The repository
foundation and first Rust shell slice are locally verified; there is not yet a
working iOS application or a feature-parity claim.

| Area | Progress |
|---|---:|
| Rust workspace | 70% |
| Shell tokenizer/parser | 63% |
| Command runtime | 82% |
| Sandboxed filesystem | 58% |
| Sessions/history | 60% |
| Configuration | 23% |
| WASM | 23% |
| Native iOS UI | 30% |
| Swift/Rust bridge | 27% |
| Package manager | 33% |
| Compatibility evidence | 2% |

## Current status

The first Rust vertical slice is implemented and locally verified. It executes
the supported built-in commands `pwd`, `cd`, `ls`, `cat`, `echo`, `mkdir`,
`touch`, `rm`, `cp`, `mv`, `env`, `export`, `unset`, `unsetenv`, `printenv`,
`setenv`, `printf`, `basename`, `dirname`, `du`, `rmdir`, `stat`, `unlink`, `tee`, `tr`, `xxd`,
`alias`, `unalias`, `find`, `sed`, `ln`, `readlink`,
`true`, `false`, `cut`, `head`, `tail`, `grep`, `sort`, `uniq`, `wc`, `wasm`, `pkg`,
`bookmark`, `showmarks`, `jump`, `renamemark`, `deletemark`, `clear`, `config`,
`help`, `history`, `uname`, `which`, `whoami`, `source`, and `.` against a
bounded filesystem,
including basic `*`/`?` pathname
expansion with quote and hidden-file rules,
with quotes, variables, leading `NAME=value` assignments, pipes, redirections,
word-boundary comments, sequencing, `&&`/`||` short-circuiting, separate
stdout/stderr, and exit status.
Assignments are expanded left-to-right, remain session-local, and can also be
issued without a command.
The text pipeline also supports a bounded literal `sed` substitution surface:
`s///` with optional `g`/`p` flags and `-n`; regular expressions and addresses
are not implemented yet.
The text pipeline also includes bounded `cut` field/character selection
(`-f`, `-c`, `-d`, and `-s`) over stdin or sandbox files.
A synchronous command response is capped at 1 MiB per output channel; a
truncation marker is emitted rather than allowing unbounded terminal output.
Individual command lines are capped at 64 KiB before parsing, and automation
scripts have their separate 256 KiB/1,024-line input boundary.
`source FILE` and `. FILE` execute bounded UTF-8 script files through the same
Rust parser, session environment, VFS, status, and history path; nested sourcing
is capped at 16 levels.
The Rust session and C/Swift bridge also expose cooperative cancellation at
command, pipeline, and script boundaries, returning status 130; an operation
already running synchronously is allowed to finish.
Environment changes are not serialized. A bounded `~/.rune_profile` is loaded
on restore; its supported Rust built-ins can update the session environment and
define aliases, with output surfaced to the CLI/native boundary without
polluting history. Alias expansion is bounded and currently accepts one
command per alias value; compound alias values are rejected explicitly. The
native source UI now asks Rust for bounded command and sandbox-path completion;
the registry and filesystem lookup remain Rust-owned and the bridge exposes only
replacement tokens.
It also has a focused command bar, keyboard-aware history controls, an
ink/cyan/ember console palette, accessible completion controls, and native
handling for the Rust `clear` screen-control sequence.
Interactive `export`, `setenv`, and assignment lines are replaced by a
redaction marker in history before persistence; this is an initial defense, not
a complete secret management policy.
The iOS app is represented by
source-only SwiftUI and FFI boundaries, but its Apple compilation, linking,
and runtime gates remain unverified. Bounded current-directory/history
persistence now exists in Rust; bounded `history-limit`, `font-size`, and
`theme` configuration is available, and the native source UI consumes the
font size and three named palettes. Configurable redaction, cursor styling,
and broader session recovery remain planned. Non-WASM language runtimes remain
planned work.

The Rust package boundary now validates a bounded, versioned JSON manifest and
checks declared file bytes with SHA-256. There is deliberately no network
transport, remote registry, or update flow yet; `pkg search` is an offline
search over manifests already installed in the sandbox.

The `wasm MODULE [arg ...]` built-in loads a module through the bounded virtual
filesystem and executes WASI preview1 `_start` in Rust. It exposes only
stdin/stdout/stderr, arguments, and the session environment; it does not
preopen a host directory. Module bytes, interpreter fuel, linear memory,
tables, and captured output are bounded. This is an initial WASM execution
slice, not a language runtime or package manager.

The local package flow supports `pkg info MANIFEST|NAME [VERSION]`, `pkg verify
MANIFEST`, `pkg install MANIFEST`, `pkg list`, `pkg search QUERY`, and `pkg
remove NAME [VERSION]`. Once installed, `pkg info NAME` resolves the sole
installed version; an explicit version is required when multiple versions are
present. Install
copies only SHA-256-verified files into `~/.rune/packages`; declared `.wasm`
commands can then run through the bounded WASI runtime, and `which` discovers
their installed command names from the local package manifests. Network
transport, registry search, and update remain unsupported at this stage.

The portable core also supports session-local virtual directory bookmarks with
`bookmark`, `showmarks`, `jump`, `cd ~NAME`, `renamemark`, and `deletemark`.
They persist with the session state and remain confined to the configured VFS;
the source-only Apple layer also handles user-selected external folders through
bounded security-scoped bookmarks. Full entitlement, picker, and device/runtime
behavior remain unverified without the Apple toolchain.
Bookmark names are limited to 64 characters, a session holds at most 256
bookmarks, and serialized bookmark data is limited to 256 KiB.

Named Rust sessions keep their virtual working directory, history, and
bookmarks independent under `~/.rune/sessions/{id}/session.state`. IDs are
validated as bounded opaque names (up to 64 ASCII characters from `A-Z`,
`a-z`, `0-9`, `_`, `-`, and `.`); they are never interpreted as shell paths.
The source-only Swift workspace uses this FFI boundary for independent
terminal tabs. Tab persistence and the Rust/FFI behavior are locally tested;
SwiftUI rendering, iPad multi-window behavior, and Apple runtime behavior
remain unverified without an Apple build toolchain.

Directory changes update the Rust-owned `PWD` and `OLDPWD` values. `cd -`
returns to the previous directory and prints the resulting virtual path, while
bookmark jumps and aliases that change directories use the same state update.

The FFI and Swift source boundary exposes bounded command and newline-delimited
script execution for Shortcuts. Source-only App Intent declarations call that
real Rust-backed API and return stdout/stderr/status as text; Rust rejects
scripts larger than 256 KiB or 1,024 lines and caps accumulated output per
channel. App Intent registration, entitlements, and runtime behavior remain
unverified without an Apple build/runtime.

The portable configuration boundary currently supports bounded `history-limit`,
`font-size`, and `theme` settings through `config get`, `config set`, and
`config reset`. It persists in `~/.rune/config.state`, and SwiftUI consumes
`font-size` plus the `ink`, `light`, and `ember` palettes; cursor styling and
toolbar preferences are not yet wired through.

The `history` built-in also supports `history N` for a bounded recent view,
`history search QUERY ...` for a case-insensitive substring search that keeps
original history numbers, and `history -c` to clear the current session
history. History records are limited by the configured count and a 4 MiB total
serialized-history budget, so a large count cannot create unbounded session
state. Consecutive duplicate entries are suppressed, while the same command
after another command remains a distinct history record.

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
- [x] Bounded script-file sourcing with nested execution limits
- [x] Cooperative command cancellation boundary
- [x] `&&` and `||` conditional chaining
- [x] Bounded recursive `find` traversal
- [x] Bounded literal `sed` substitutions
- [x] Bounded terminal output channels
- [x] Session-local virtual directory bookmarks
- [x] Bounded portable utility commands
- [x] Bounded virtual filesystem metadata and usage commands

### Phase 3 — Developer environment

- [x] Bounded WASI preview1 runtime boundary and resource limits
- [x] Bounded package metadata, integrity, and local WASM installation
- [x] Portable runtime request/output contract
- [x] Bounded Rust-owned history, font-size, and theme configuration
- [ ] Network registry, search, and update policy
- [ ] Python, JavaScript, and Lua runtime evaluation
- [x] Rust-owned command/path completion and help metadata

### Phase 4 — Apple integration

- [ ] Fast native terminal rendering
- [x] Source-only external folders and bounded security-scoped bookmarks
- [x] Rust-namespaced sessions and source-only terminal tabs
- [ ] iPad multi-window behavior
- [x] Source-only command/script App Intent declarations
- [ ] Apple Shortcuts actions
- [ ] Accessibility and VoiceOver validation

## Non-goals for the current milestone

- claiming a-Shell parity
- copying a-Shell implementation or UI
- executing arbitrary host processes from the shell
- pretending an iOS app exists before it is built and tested
