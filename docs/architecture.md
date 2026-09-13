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
- `rune-core`: command registry, command context, session state, execution
  results, and the bounded first-word completion query used by native
  frontends.
- `rune-wasm`: bounded WASI preview1 execution with no host-directory
  preopens in the initial slice.
- `rune-package`: bounded versioned manifest parsing and SHA-256 artifact
  verification; transport and installation are intentionally outside this
  first boundary.
- `rune-runtime`: runtime-neutral request/output/error contracts. It does not
  ship language interpreters; providers are added behind this boundary.
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

A synchronous command response is capped at 1 MiB per stdout or stderr
channel. The cap is applied after redirections, so terminal rendering cannot
receive unbounded output; a visible truncation marker is emitted and the
underlying command status is preserved.

Execution plans preserve `;`, `&&`, and `||` as connectors between pipelines.
The core evaluates them left-to-right and skips only the next pipeline when
the connector's status condition is not met; a skipped branch does not invent
output or change the previous status.

The tokenizer treats an unquoted `#` at a word boundary as the beginning of a
comment and stops lexing the remainder of that line. Hashes inside a word,
inside quotes, or escaped remain literal, so startup profiles can use ordinary
comments without weakening argument handling.

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

Configuration is a separate, versioned Rust-owned file at
`~/.rune/config.state`. The current schema contains only a validated
`history_limit` between 1 and 10,000; `config get`, `config set`, and
`config reset` update it and the session applies the limit immediately. Visual
preferences are intentionally not serialized until the native UI consumes a
defined configuration contract.

The `history` built-in can render the full session, a bounded recent count, or
clear the mutable history with `history -c`. The command is still subject to
the configured history limit when new entries are recorded.

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
`sed`, `sort`, `uniq`, and `wc`). They consume the same in-memory pipeline stdin
as file commands and never delegate to a host shell. `sed` currently supports
literal `s///` substitutions with `g` and `p` flags plus `-n`; it does not claim
regular-expression or address compatibility. These option surfaces are
deliberately smaller than POSIX utilities until compatibility tests justify
expanding them.

The `find` built-in walks the same virtual filesystem through `metadata` and
`list`; it never traverses the host root directly. Its initial surface supports
one start path, `-name` basename matching, and `-maxdepth`. Traversal is capped
at 10,000 visited entries and does not follow symlink entries, keeping a
malicious or cyclic tree from turning a synchronous command into unbounded
work.

The portable utility slice adds bounded `basename`, `dirname`, `du`, `rmdir`,
`stat`, `unlink`, `tee`, `tr`, and `xxd` commands, plus `ln -s`/`readlink` and
the `unsetenv` spelling for environment removal. They operate on Rune's virtual filesystem or pipeline
stdin only; they do not invoke host executables. `tr` supports literal Unicode
character translation/deletion, while `xxd` supports plain and classic hex
output with a 256 KiB input limit. `cp -r` copies regular-file directory trees
with a 10,000-entry limit and rejects symlinks; `mv` can move a directory
without recursively traversing it. `du` reports a recursive byte total with a
10,000-entry limit, and `stat` reports only metadata available through the VFS.

`ln -s` accepts only an existing relative target and verifies that the target
resolves inside the sandbox before creating the link. `readlink` exposes the
stored relative target, while ordinary recursive operations do not follow
symlinks.

The shell also provides deterministic `uname` and `whoami` identities for the
portable session, and `which` reports aliases and registered built-ins. These
commands never expose the host user's name or claim that arbitrary host
executables are available; installed package discovery will be added when the
package command index is defined.

The Rust shell includes a bounded `printf` formatter for `%s`, `%c`, `%d`,
`%i`, `%%`, and the `\\n`, `\\r`, `\\t`, and `\\\\` escapes. It deliberately
rejects unsupported conversions and malformed integer arguments instead of
delegating formatting to a host shell.

Session-local virtual bookmarks are stored as validated names mapped to Rune
virtual directories. `bookmark`, `showmarks`, `jump`, `renamemark`, and
`deletemark` mutate or inspect that map; `cd ~NAME` resolves an existing mark
before asking the VFS to change directory. The map is persisted with the
bounded session state. It is not an Apple security-scoped bookmark and cannot
grant access outside the configured VFS.

The `wasm MODULE [arg ...]` built-in reads the module through the virtual
filesystem and invokes WASI preview1 `_start` in the Rust runtime. The guest
receives only argv, the session environment, stdin, stdout, and stderr. The
initial linker does not expose a preopened directory or host process API, so
WASI filesystem access is unavailable until it can be mapped to explicit Rune
capabilities. Each invocation bounds module bytes, interpreter fuel, linear
memory, tables, and captured output. Guest traps become a failed command while
preserving captured output; explicit WASI exits preserve their exit status.

Package metadata is parsed independently of network transport through
`rune-package`. Schema version 1 rejects unknown fields, path traversal,
duplicate entries, undeclared command targets, malformed digests, and
path-unsafe package versions. Declared file bytes are checked with SHA-256
before the local installer accepts them.
This is integrity evidence, not a signature or publisher-trust system; signed
repositories and installation policy remain future work.
The `pkg info` and `pkg verify` built-ins expose only local manifest inspection
and verification through the VFS. `pkg install` copies a verified manifest and
its declared files into the bounded `~/.rune/packages` tree; `pkg list` reads
those installed manifests and `pkg remove` deletes an explicitly named package
or version. Only declared `.wasm` command entries are executable today, and
installed module bytes are verified again before execution. There is no
network client or registry yet.

The runtime contract is owned by Rust and carries only explicit program bytes,
arguments, environment, and stdin into a provider. `rune-wasm` implements the
first provider by adapting its bounded WASI result to that contract. Python,
JavaScript, and Lua remain unimplemented rather than being represented by
placeholder execution.

Unquoted `*` and `?` are expanded by `rune-core` through the VFS `glob` method;
quoted patterns remain literal, hidden entries require a leading `.`, and an
unmatched pattern remains a literal argument. The VFS validates every matched
candidate against its canonical root before returning it.

Apple source integration will be added without installing a new Xcode or
simulator footprint. Until an existing Apple toolchain is explicitly used,
Swift compilation and runtime behavior remain unverified gates rather than
assumed capabilities.

Automation uses the same ownership boundary: `Session::execute_script` runs
non-empty newline-delimited lines through the Rust parser and returns combined
stdout/stderr plus the last status. `rune-ffi` and the Swift source bridge
expose that method for a future Apple Shortcuts adapter; they do not register
an Intent or claim Shortcuts runtime compatibility yet.
