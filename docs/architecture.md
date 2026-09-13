# Rune architecture

## Direction

Rune is being built as an independent Rust-native terminal core with a native
Apple frontend. The core must be useful and testable without UIKit or SwiftUI;
the frontend will translate user input and streamed command events across a
narrow, versioned boundary.

The local a-Shell checkout is used to understand user-visible behavior and iOS
constraints. It is not a source dependency or an implementation template.

## Initial crate boundaries

- `rune-shell`: lexical and syntactic shell concerns. It produces explicit
  execution plans rather than executing commands.
- `rune-fs`: path resolution and filesystem policy. Commands receive this
  abstraction rather than reaching into Apple APIs directly.
- `rune-core`: command registry, command context, session state, and execution
  results.
- `rune-ffi`: a deliberately narrow C ABI for opaque session handles and owned
  stdout/stderr buffers. Its unsafe code is isolated at the boundary.
- `apps/rune-cli`: a small host executable used for local development and
  end-to-end checks. It is not the iOS frontend.

These boundaries are deliberately small. New crates should be added only when
they own a coherent capability with tests.

## Runtime direction

The first execution engine is synchronous and deterministic so behavior can be
tested easily. Its result model already separates stdout, stderr, and exit
status. Later async execution and streaming can be introduced behind the same
conceptual event boundary once iOS cancellation and rendering requirements are
specified.

Session persistence is explicit and intentionally narrow: `~/.rune/session.state`
stores the virtual working directory and command history, while environment
variables are reconstructed for every session and are never serialized. The
current shell environment can be changed by the Rust `export`, `unset`, and
`setenv` built-ins, or by leading `NAME=value` assignments. Assignments are
expanded from the current environment in left-to-right order, remain
session-local, and may be used without a command. The initial history policy
replaces parsed `export`, `setenv`, and assignment lines with
`[redacted environment assignment]` before storage. The command still executes
with its real value in memory. This is only a narrow first defense; configurable
redaction is still required before Rune handles workflows where users type
credentials into arbitrary commands.

Aliases live in the same session boundary but are not serialized. `alias` and
`unalias` mutate the Rust-owned alias map, so profile commands can establish
repeatable local shortcuts without Swift-specific state. Before command lookup,
Rune parses a matching alias value and merges its assignments, arguments, and
redirections with the invocation. Each alias value is limited to one command;
recursive expansion is capped at 32 levels and compound values fail with a
normal shell error instead of recursing indefinitely.

On restore, Rune reads at most 64 KiB from `~/.rune_profile`. It skips blank and
full-line comment entries, executes each remaining line through the same Rust
parser/registry, and returns profile stdout/stderr through the CLI or FFI. The
profile is loaded before the persisted working directory is restored, so a
session's saved `cwd` remains authoritative. Profile lines are not added to
history, and unsupported commands fail visibly instead of reaching the host.

The filesystem starts with a host-backed root for local development. The root
is a policy boundary: paths are resolved relative to it, `~` maps to the root,
and traversal outside the root is rejected. An Apple adapter will map that
root to the app's Documents directory and add user-approved external roots
through security-scoped bookmarks.

The current registry includes bounded text filters (`head`, `tail`, `grep`,
`sort`, `uniq`, and `wc`). They consume the same in-memory pipeline stdin as
file commands and never delegate to a host shell. Their option surfaces are
deliberately smaller than POSIX utilities until compatibility tests justify
expanding them.

Unquoted `*` and `?` are expanded by `rune-core` through the VFS `glob` method;
quoted patterns remain literal, hidden entries require a leading `.`, and an
unmatched pattern remains a literal argument. The VFS validates every matched
candidate against its canonical root before returning it.

Apple source integration will be added without installing a new Xcode or
simulator footprint. Until an existing Apple toolchain is explicitly used,
Swift compilation and runtime behavior remain unverified gates rather than
assumed capabilities.
