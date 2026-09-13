# Rune

Rune is a Rust-native Unix-like terminal environment for iPhone and iPad, with
a first-class native Apple frontend planned around Swift and SwiftUI. It is an
independent implementation, designed from first principles for iOS sandbox
constraints, reliable command execution, and portable core behavior.

The local `base/a-shell/` checkout is a behavioral and product reference. It is
excluded from Git and no a-Shell source is part of Rune.

## Development Progress

**Overall progress: 17%**

This is an intentionally conservative engineering estimate. The repository
foundation and first Rust shell slice are locally verified; there is not yet a
working iOS application or a feature-parity claim.

| Area | Progress |
|---|---:|
| Rust workspace | 70% |
| Shell tokenizer/parser | 58% |
| Command runtime | 61% |
| Sandboxed filesystem | 43% |
| Sessions/history | 32% |
| WASM | 0% |
| Native iOS UI | 9% |
| Swift/Rust bridge | 6% |
| Package manager | 0% |
| Compatibility evidence | 2% |

## Current status

The first Rust vertical slice is implemented and locally verified. It executes
the supported built-in commands `pwd`, `cd`, `ls`, `cat`, `echo`, `mkdir`,
`touch`, `rm`, `cp`, `mv`, `env`, `export`, `unset`, `printenv`, `setenv`,
`alias`, `unalias`,
`true`, `false`, `head`, `tail`, `grep`, `sort`, `uniq`, `wc`, `clear`, `help`,
and `history` against a bounded filesystem, including basic `*`/`?` pathname
expansion with quote and hidden-file rules,
with quotes, variables, leading `NAME=value` assignments, pipes, redirections,
sequencing, separate stdout/stderr, and exit status. Assignments are expanded
left-to-right, remain session-local, and can also be issued without a command.
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
recovery remain planned. WASM and package management remain planned work.

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

### Phase 3 — Developer environment

- [ ] WASM runtime boundary and resource limits
- [ ] Package metadata and verification
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
