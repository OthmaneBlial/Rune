# Rune

Rune is a Rust-native Unix-like terminal environment for iPhone and iPad, with
a first-class native Apple frontend planned around Swift and SwiftUI. It is an
independent implementation, designed from first principles for iOS sandbox
constraints, reliable command execution, and portable core behavior.

The local `base/a-shell/` checkout is a behavioral and product reference. It is
excluded from Git and no a-Shell source is part of Rune.

## Development Progress

**Overall progress: 4%**

This is an intentionally conservative engineering estimate. The repository
foundation and the first Rust shell slice are in progress; there is not yet a
working iOS application or a feature-parity claim.

| Area | Progress |
|---|---:|
| Rust workspace | 35% |
| Shell tokenizer/parser | 0% |
| Command runtime | 0% |
| Sandboxed filesystem | 0% |
| Sessions/history | 0% |
| WASM | 0% |
| Native iOS UI | 0% |
| Swift/Rust bridge | 0% |
| Package manager | 0% |
| Compatibility evidence | 1% |

## Current status

The current milestone is the Rust foundation and a real, host-runnable shell
vertical slice. It will execute supported built-in commands against a bounded
filesystem, return separate stdout/stderr and an exit status, and provide a
small CLI for local verification. The iOS app, FFI surface, persistence,
WASM, and package management remain planned work.

No a-Shell compatibility area is marked `supported` without behavior and test
evidence. See [`compat/a-shell-compatibility.json`](compat/a-shell-compatibility.json).

## Architecture

```text
Swift / SwiftUI app (planned)
            │ narrow FFI boundary (planned)
            ▼
      rune-core  ─── command registry and session orchestration
        │   │
        │   └──── rune-fs   bounded filesystem abstraction
        └──────── rune-shell tokenizer, parser, execution plan
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

The reference checkout is intentionally ignored and can be checked with:

```bash
git check-ignore -v base/a-shell
```

## Roadmap

### Phase 1 — Foundation

- [x] Repository and Rust workspace
- [x] Local-only quality workflow
- [ ] First command execution slice
- [ ] SwiftUI iOS application shell
- [ ] Stable Rust/Swift bridge

### Phase 2 — Core shell

- [ ] Tokenizer and parser
- [ ] Environment and path expansion
- [ ] Bounded virtual filesystem
- [ ] Built-in file commands
- [ ] Sessions and persisted history
- [ ] Pipes and redirections

### Phase 3 — Developer environment

- [ ] WASM runtime boundary and resource limits
- [ ] Package metadata and verification
- [ ] Python, JavaScript, and Lua runtime evaluation
- [ ] Completion and help system

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
