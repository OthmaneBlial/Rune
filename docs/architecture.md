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
  results, and the bounded command/path completion query used by native
 frontends.
- `rune-wasm`: bounded WASI preview1 execution with optional explicit
  capability-scoped preopens supplied by the session VFS.
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
status. An event sink can receive bounded output after each completed pipeline
and a status/directory event at each command boundary; the C ABI forwards this
as borrowed callback data for Swift to copy. This is boundary-level event
delivery, not live UI rendering or byte-level streaming from an in-flight WASM
call.

A synchronous command response is capped at 1 MiB per stdout or stderr
channel, and the same bound is applied between pipeline stages. The cap is
applied after redirections, so terminal rendering cannot receive unbounded
output; a visible truncation marker is emitted and the underlying command
status is preserved. Individual command lines are capped at 64 KiB before
parsing; automation scripts have separate 256 KiB and 1,024-line limits.

Session cancellation is cooperative. Rust owns an atomic cancellation request,
observes it before commands and between pipelines/script lines, and bounded
filesystem/archive traversals also poll it. The bounded `sleep` builtin polls
the flag in 25 ms intervals. The flag is cleared when reported, and cancellation
returns the conventional status 130. The C bridge exposes the same request for
a native cancellation callback. A synchronous filesystem or runtime operation
already in progress is not forcefully interrupted, and Rune does not claim host
signal delivery on iOS yet.

Execution plans preserve `;`, `&&`, and `||` as connectors between pipelines.
The core evaluates them left-to-right and skips only the next pipeline when
the connector's status condition is not met; a skipped branch does not invent
output or change the previous status.

The tokenizer treats an unquoted `#` at a word boundary as the beginning of a
comment and stops lexing the remainder of that line. Hashes inside a word,
inside quotes, or escaped remain literal, so startup profiles can use ordinary
comments without weakening argument handling.

Session persistence is explicit and intentionally narrow: the legacy/default
session stores the virtual working directory, command history, and bounded
bookmarks in `~/.rune/session.state`. Named sessions use
`~/.rune/sessions/{id}/session.state` instead, so tabs can restore independent
cwd/history/bookmark state without sharing records. Session IDs are opaque,
validated names of at most 64 ASCII alphanumeric, `_`, `-`, or `.` characters;
they are never resolved as filesystem input. Environment variables are
reconstructed for every session and are never serialized. The
current shell environment can be changed by the Rust `export`, `unset`, and
`setenv` built-ins, or by leading `NAME=value` assignments. Directory changes
maintain `PWD` and `OLDPWD`; `cd -` returns to the previous virtual directory
and prints it. Assignments are
expanded from the current environment in left-to-right order, remain
session-local, and may be used without a command. The initial history policy
replaces parsed `export`, `setenv`, and assignment lines with
`[redacted environment assignment]` before storage. The command still executes
with its real value in memory. This is only a narrow first defense; configurable
redaction is still required before Rune handles workflows where users type
credentials into arbitrary commands.

Configuration is a separate, versioned Rust-owned file at
`~/.rune/config.state`. The current schema contains a validated `history_limit`
between 1 and 10,000, a `font_size` between 8 and 32 points, a
`scrollback_limit` between 128 and 8,192 rendered entries, a font design in
`monospaced`, `system`, or `rounded`, a theme in `ink`, `light`, or `ember`, and
a cursor color in `cyan`, `ember`, or `foreground`.
It also supports independent background overrides in `auto`, `black`, `white`,
or `slate`, and foreground overrides in `auto`, `black`, `white`, `cyan`, or
`ember`.
`config get`, `config set`, and `config reset` update these
values, and the session applies them immediately. SwiftUI consumes all eight
settings; the public session and C ABI also expose validated set/reset
operations that do not add shell text to history, allowing native settings
surfaces to use the same Rust policy. The source-only SwiftUI sheet uses that
boundary for font, font size, scrollback, theme, cursor color, background,
foreground, reset, and toolbar
visibility; its scrollback window also remains subject to an 8 MiB byte cap.
Cursor shape remains outside the current contract.

The `history` built-in can render the full session, a bounded recent count,
search matching entries with `history search QUERY ...`, or clear the mutable
history with `history -c`. Search is case-insensitive, retains original entry
numbers, and is limited to 256 query characters. New records obey both the
configured count and a 4 MiB serialized-history budget, preventing a large
count from creating unbounded session state. Consecutive duplicate records are
suppressed before those limits are applied; non-consecutive repeats remain
distinct.

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

Automation and script files share one execution path. `Session::execute_script`
accepts bounded newline-delimited input, while the Rust `source FILE [ARG ...]`
and `. FILE [ARG ...]` built-ins read a UTF-8 file through the VFS and send it
through the same parser, command registry, environment, status, and history
handling. Sourced files expose the path and arguments as `$0`, `$1...`, `$#`,
and `$@`, with at most 64 arguments; nested parameters are restored on return.
Stdin from the enclosing pipeline is preserved for the script's first command.
Sourced files inherit the current virtual directory and session state; they do
not invoke a host shell. Each sourced file is limited to 256 KiB and 1,024
lines, and nested sourcing stops at 16 levels with a status-2 error.

The Apple source layer declares command and script App Intents that construct a
normal `RuneFFISession` rooted at the app Documents directory and return the
Rust result without reimplementing command behavior in Swift. This is a real
automation boundary, but App Intent registration, entitlements, and Shortcuts
runtime execution remain unverified until an Apple target can be built.

Native file automation uses `Session::read_file` and `Session::write_file`, not
shell-string interpolation. Both operations stay inside the session VFS and
enforce a separate 16 MiB transfer limit; writes replace one file and do not
create parent directories. The FFI returns binary reads with an explicit
pointer/length release function, so NUL bytes are preserved. Source-only
Shortcuts currently expose the safer UTF-8 text Put/Get surface, while the
binary-safe FFI remains available for a future validated native file type.

The source-only workspace tab layer creates the default session for the first
tab and named Rust sessions for additional tabs. Swift owns tab selection and
presentation; Rust owns each tab's shell state and persistence. Swift persists
only bounded tab metadata in UserDefaults, excluding external paths and Apple
bookmark bytes. The named WindowGroup accepts a typed window route: each
additional iPad window receives its own Rust session namespace and its own
bounded tab metadata key, while the default window preserves the legacy key.
This is source/API evidence for routing; it does not yet prove SwiftUI
lifecycle behavior, scene restoration, or device runtime behavior.

Interactive command calls use a lock-protected Swift FFI handle and run away
from the SwiftUI main actor. The cancellation method intentionally bypasses
that lock and signals Rust's atomic request flag; the next command, pipeline,
or script boundary returns status 130. This keeps the UI callback responsive
without pretending that a synchronous filesystem or runtime operation can be
forcefully interrupted.

The Swift transcript is a separate presentation cache with a configurable
4,096-entry default, an 8,192-entry maximum, and an 8 MiB UTF-8 text cap. It
evicts oldest rendered events at the boundary;
Rust output limits, command history, and sandbox files remain independent of
that UI eviction policy. The source-only Apple renderer consumes common ANSI
SGR foreground colors, bold, and underline controls after the Rust boundary;
unsupported terminal controls are deliberately bounded and not claimed as a
complete emulator.

The terminal view also declares native keyboard shortcuts for folder import,
cooperative cancellation, history navigation, command submission, and a
source-only settings sheet. An optional bounded input toolbar adds
Tab/completion, Escape, Ctrl-C, display-clear, and paste controls. These
controls dispatch into the existing Swift model and Rust FFI rather than
maintaining a second command, configuration, or history implementation; the
toolbar visibility preference is persisted by Rust and surfaced through the
same FFI. Their behavior still needs Apple keyboard/runtime validation.

The filesystem starts with a host-backed root for local development. The root
is a policy boundary: paths are resolved relative to it, `~` maps to the root,
and traversal outside the root is rejected. An Apple adapter will map that
root to the app's Documents directory and add user-approved external roots
through security-scoped bookmarks. The source-only Apple layer stores at most
128 named bookmark records and 512 KiB of bookmark data, resolves stale records,
keeps each security scope alive for its active Rust session, and passes Rust
only the approved root URL. Entitlements and runtime access remain unverified.
Regular-file reads, appends, and copies are bounded at 64 MiB before their
contents are allocated or duplicated; directory listing and wildcard
enumeration are each capped at 10,000 entries. These limits are separate from
the terminal and native-transfer limits.

The current registry includes bounded text filters (`cut`, `head`, `tail`,
`grep`, `sed`, `sort`, `uniq`, and `wc`). They consume the same in-memory pipeline stdin
as file commands, accept `-` as an explicit stdin operand, and never delegate
to a host shell. `sed` currently supports
literal `s///` substitutions with `g` and `p` flags plus `-n`; it does not claim
regular-expression or address compatibility. These option surfaces are
deliberately smaller than POSIX utilities until compatibility tests justify
expanding them.
`grep` performs bounded literal matching and supports `-i`, `-v`, `-n`, and
`-c`, retaining status 0/1 match semantics and status 2 for usage errors.

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

`ls` supports bounded `-l` and `-h` views in addition to hidden-entry flags.
Long output uses a compact type marker (`d`, `-`, or `l`) and the VFS-reported
size; permissions, ownership, and timestamps are intentionally absent because
the portable boundary does not expose them.

`ln -s` accepts only an existing relative target and verifies that the target
resolves inside the sandbox before creating the link. `readlink` exposes the
stored relative target, while ordinary recursive operations do not follow
symlinks.

The host-backed VFS can model the standard Apple app layout with Documents as
the virtual home (`~`), plus separately approved `Library` and `tmp` mounts at
`~/Library` and `~/tmp`. The mount roots are canonicalized independently,
reserved root operations are rejected, `..` at a mount boundary returns to the
virtual home, and symlink targets may resolve only inside one of the approved
roots. External-folder sessions use the single selected folder as their root;
they do not gain access to the app's Library or temporary directory.

The shell also provides deterministic `uname` and `whoami` identities for the
portable session, and `which` reports aliases and registered built-ins. These
commands never expose the host user's name or claim that arbitrary host
executables are available; installed package commands are discovered from
local package manifests and reported with their package and version.

Native completion asks the Rust session for replacement tokens. At the start
of a line it returns built-in and installed-package command names; for
supported path-oriented
commands it lists only entries in the bounded VFS, preserves virtual prefixes
such as `~/` and `../`, marks directories with `/`, and caps results at eight.
Quoted, escaped, option, and compound-shell fragments are intentionally
deferred until the completion grammar has structured replacement ranges.

The Rust shell includes a bounded `printf` formatter for `%s`, `%c`, `%d`,
`%i`, `%%`, and the `\\n`, `\\r`, `\\t`, and `\\\\` escapes. It deliberately
rejects unsupported conversions and malformed integer arguments instead of
delegating formatting to a host shell.

Session-local virtual bookmarks are stored as validated names mapped to Rune
virtual directories. `bookmark`, `showmarks`, `jump`, `renamemark`, and
`deletemark` mutate or inspect that map; `cd ~NAME` resolves an existing mark
before asking the VFS to change directory. Names are limited to 64 characters,
each session holds at most 256 marks, and serialized bookmark data is limited
to 256 KiB. The map is persisted with the bounded session state. It is not an
Apple security-scoped bookmark and cannot grant access outside the configured
VFS.

The `wasm MODULE [arg ...]` built-in reads the module through the virtual
filesystem and invokes WASI preview1 `_start` in the Rust runtime. The guest
receives argv, the session environment, stdin, and stderr, plus explicit
preopens when the VFS exposes approved host roots. Each preopen is opened
through capability-based APIs and cannot grant access beyond Rune's sandbox;
host process and network APIs are not linked. Each invocation
bounds module bytes, interpreter fuel, linear memory, tables, arguments,
environment, stdin, and captured output before guest setup. Guest traps become
a failed command while preserving captured output;
explicit WASI exits preserve their exit status. Fine-grained per-operation
WASI rights and additional runtime families remain planned. Session-backed
multi-root Apple layouts expose Documents/home at `/`, Library at `/Library`,
and tmp at `/tmp`; single-root and external-folder sessions expose only `/`.
requests pass the atomic cancellation boundary to the provider; WASM consumes
it before execution or when a fuel stop is observed and returns status 130 with
a diagnostic. A call that finishes before an observation may complete
normally, matching the cooperative contract.

The VFS exposes its host root to this boundary only as an optional borrowed
capability. Sandboxed host VFS instances return their canonical root; other
VFS implementations return no root and therefore keep WASI filesystem access
disabled. Swift does not receive or construct this path: approved external
folder access must enter through the Rust VFS boundary first.

Package metadata is parsed independently of network transport through
`rune-package`. Schema version 1 rejects unknown fields, path traversal,
duplicate entries, undeclared command targets, malformed digests, and
path-unsafe package versions. Declared file bytes are checked with SHA-256
before the local installer accepts them.
This is integrity evidence, not a signature or publisher-trust system; signed
repositories and installation policy remain future work.

The archive command layer implements a deliberately narrow ZIP32 profile. ZIP
creation writes stored UTF-8 entries and CRC32 values through the VFS; recursive
directory traversal is bounded to 10,000 entries and the complete archive to
64 MiB. Extraction reads the central directory, rejects encryption,
compression, data descriptors, multi-disk records, duplicate names, absolute
paths, and dot or parent components, then verifies each local entry's name,
bounds, and CRC before writing it into the confined destination. ZIP64,
compression, and broad external compatibility are not claimed.
The `pkg info` and `pkg verify` built-ins expose only local manifest inspection
and verification through the VFS. `pkg info NAME [VERSION]` can also resolve
an installed manifest, but refuses an unversioned lookup when multiple
versions are present. `pkg install` copies a verified manifest and
its declared files into the bounded `~/.rune/packages` tree; `pkg list` reads
those installed manifests and `pkg remove` deletes an explicitly named package
or version. `pkg update MANIFEST` verifies a different local version, writes it
alongside the current version, and removes the old version only after the new
tree is complete; verification or materialization failure leaves the old
version installed. Declared `.wasm` command entries run through the bounded
WASI provider; declared `.rune` entries run through the same Rust parser and
script limits as `source`. Installed module/script bytes are verified again
before execution. There is no
network client or remote registry. `pkg search QUERY` performs a bounded,
case-insensitive search over installed package names, versions,
descriptions, and command names. Installed WASM commands receive no filesystem
preopen by default. A manifest must explicitly declare
`permissions.filesystem: true` before that command can receive the approved
Rune sandbox as `/`; unknown capability fields are rejected. Network access is
not a package capability.

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
stdout/stderr plus the last status. The input is limited to 256 KiB and 1,024
lines; accepted scripts cap accumulated output per channel after each line.
`rune-ffi` and the Swift source bridge expose that method for a future Apple
Shortcuts adapter; they do not register an Intent or claim Shortcuts runtime
compatibility yet.
