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
  results, bounded command/path completion queries, and Rust-owned completion
  replacement used by native frontends.
- `rune-wasm`: bounded WASI preview1 execution with optional explicit
  capability-scoped preopens supplied by the session VFS.
- `rune-package`: bounded versioned manifest parsing and SHA-256 artifact
  verification; transport and installation are intentionally outside this
  first boundary.
- `rune-runtime`: runtime request/output/error contracts plus bounded Lua 5.4
  and JavaScript providers. WASM stays in its dedicated crate; other providers
  are added behind the same boundary.
- `rune-ffi`: a deliberately narrow C ABI for opaque session handles and owned
  stdout/stderr buffers. Its unsafe code is isolated at the boundary.
- `apps/rune-cli`: a small host executable used for local development and
  end-to-end checks. It is not the iOS frontend.

These boundaries are deliberately small. New crates should be added only when
they own a coherent capability with tests.

## Runtime direction

The first execution engine is synchronous and deterministic so behavior can be
tested easily. Its result model already separates stdout, stderr, and exit
status. An event sink can receive bounded 16 KiB maximum UTF-8 output chunks
after each completed pipeline and a status/directory event at each command
boundary; the C ABI forwards this
as borrowed callback data for Swift to copy. The source-only terminal model
feeds those copies through an `AsyncStream` so the main actor can render each
completed pipeline while execution is still in progress. This is
boundary-level event delivery, not byte-level streaming from an in-flight WASM
call; Apple runtime behavior remains unverified.

A synchronous command response is capped at 1 MiB per stdout or stderr
channel, and the same bound is applied between pipeline stages. The cap is
applied after redirections, so terminal rendering cannot receive unbounded
output; a visible truncation marker is emitted and the underlying command
status is preserved. Event delivery then splits each visible channel into
UTF-8-safe 16 KiB chunks without changing the aggregate response. Individual
command lines are capped at 64 KiB before
parsing; automation scripts have separate 256 KiB and 1,024-line limits.
The shell plan also models `2>&1`, `1>&2`, `>&2`, and `&>`/`&>>` as ordered
stream targets. Duplicated streams share one bounded file write or captured
channel; because the core stores stdout and stderr separately, a merged result
is deterministic stdout-then-stderr text rather than a byte-level interleave.

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

The tokenizer also records bounded `$(...)` command substitutions, including
nested parentheses outside quoted inner text. `rune-core` executes each
substitution through its ordinary parsed plan with no history record, removes
trailing newlines from captured stdout, and restores the inherited shell state
afterward. VFS writes made by the substitution are intentionally not rolled
back; the isolation applies to cwd, environment, aliases, bookmarks,
configuration, history, status, and script parameters.

Session persistence is explicit and intentionally narrow: the legacy/default
session stores the virtual working directory, command history, and bounded
bookmarks in `~/.rune/session.state`. A separate `~/.rune/terminal.state`
stores only a bounded recent visible-text window and zero-based cursor
position. Named sessions use matching `session.state` and `terminal.state`
files below `~/.rune/sessions/{id}/`, so tabs can restore independent
cwd/history/bookmark/terminal state without sharing records. Styles, scroll
margins, and incomplete control sequences are intentionally not persisted.
Session IDs are opaque,
validated names of at most 64 ASCII alphanumeric, `_`, `-`, or `.` characters;
they are never resolved as filesystem input. Environment variables are
reconstructed for every session by default and are never serialized unless
the explicit `environment-persistence` setting is enabled. In that opt-in
mode, only bounded user-defined names are restored; `HOME`, `PATH`,
`RUNE_VERSION`, `TERM`, `PWD`, and `OLDPWD` remain process-owned and are never
taken from disk. The current shell environment can be changed by the Rust `export`, `unset`, and
`setenv` built-ins, or by leading `NAME=value` assignments. Directory changes
maintain `PWD` and `OLDPWD`; `cd -` returns to the previous virtual directory
and prints it. Assignments are
expanded from the current environment in left-to-right order, remain
session-local, and may be used without a command. The initial history policy
replaces parsed `export`, `setenv`, and assignment lines with
`[redacted environment assignment]` before storage. The command still executes
with its real value in memory. This narrow detector is configurable through the
Rust-owned `history-redaction` setting and does not claim to detect secrets
embedded in every arbitrary command.

Configuration is a separate, versioned Rust-owned file at
`~/.rune/config.state`. The current schema contains a validated `history_limit`
between 1 and 10,000, a `history_redaction` boolean enabled by default, a
`font_size` between 8 and 32 points, a
`scrollback_limit` between 128 and 8,192 rendered entries, an
`environment_persistence` boolean disabled by default, a font design in
`monospaced`, `system`, or `rounded`, a theme in `ink`, `light`, or `ember`, and
a cursor color in `cyan`, `ember`, or `foreground`, and a cursor shape in
`bar`, `block`, or `underline`.
It also supports independent background overrides in `auto`, `black`, `white`,
or `slate`, and foreground overrides in `auto`, `black`, `white`, `cyan`, or
`ember`.
`config get`, `config set`, and `config reset` update these
values, and the session applies them immediately. Setting `history-redaction`
to false is an explicit opt-out that may persist secrets. Setting
`environment-persistence` to true is a separate explicit opt-in that may store
exported values; it is false by default and bounded to user-defined variables.
SwiftUI consumes all twelve
settings; the public session and C ABI also expose validated set/reset
operations that do not add shell text to history, allowing native settings
surfaces to use the same Rust policy. The source-only SwiftUI sheet uses that
boundary for font, font size, scrollback, theme, cursor color, cursor shape,
background, foreground, history redaction, reset, and toolbar visibility; its scrollback window
also remains subject to an 8 MiB byte cap. The source-only UIKit command editor
maps the validated shape to bar, block, or underline caret geometry when UIKit
is available; Apple compilation and runtime rendering remain unverified.

The `history` built-in can render the full session, a bounded recent count,
search matching entries with `history search QUERY ...`, or clear the mutable
history with `history -c`. Built-in search is case-insensitive, retains
original entry numbers, and is limited to 256 query characters. The native
reverse-search API uses the same Rust-owned history, bounds its query to 4 KiB,
returns newest-first matches, and does not record a synthetic command. New
records obey both the configured count and a 4 MiB serialized-history budget,
preventing a large count from creating unbounded session state. Consecutive
duplicate records are suppressed before those limits are applied;
non-consecutive repeats remain distinct.

Aliases live in the same session boundary but are not serialized. `alias` and
`unalias` mutate the Rust-owned alias map, so profile commands can establish
repeatable local shortcuts without Swift-specific state. Before command lookup,
Rune parses a matching alias value and merges its assignments, arguments, and
redirections with the invocation. Each alias value is limited to one command;
recursive expansion is capped at 32 levels and compound values fail with a
normal shell error instead of recursing indefinitely.

On restore, Rune reads at most 64 KiB from the first existing startup profile
in this order: `~/.rune_profile`, `~/.profile`, then `~/.bashrc`. It skips blank
and full-line comment entries, joins the remaining content into one bounded
script, and executes it through the same Rust parser/registry. This lets
multiline control flow and function definitions participate in startup while
keeping the script bounded to 256 KiB and 1,024 lines. Profile stdout/stderr is
returned through the CLI or FFI. The profile is loaded before the persisted
working directory is restored, so a session's saved `cwd` remains authoritative.
Profile content is not added to history, and unsupported commands fail visibly
instead of reaching the host.

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
`sh -c SCRIPT` and `dash -c SCRIPT` use the same Rust parser and script
executor for inline scripts. An optional command name becomes `$0`, following
arguments become bounded `$1...` values, and stdin/redirections remain inside
the current virtual session; no host shell or process is started. Shell-script
nesting shares the 16-level source limit and accepts at most 64 positional
arguments. Multiline scripts also have a bounded `for NAME in VALUE ...; do`
construct. Its expanded values and nested bodies are executed through this same
Rust planner; each loop accepts at most 256 values and leaves its final loop
variable in the session environment. Multiline `if/elif/else/fi` branches use
the same planner for their conditions and selected body, with control-flow
nesting capped at 16 levels. Multiline `while` and `until` loops use the same
condition/body path and stop after 1,024 body iterations. Multiline or bounded inline
`NAME() { ... }` function definitions and calls use the same Rust planner,
share the current session state, preserve bounded positional parameters, and
cap the session at 256 definitions, 64 call arguments, and 16 recursive calls.
Function definitions and calls are isolated from nested `sh -c` and command
substitution execution. Inline function bodies use the same Rust planner and
semicolon-separated command sequencing. One-line loop bodies remain outside the
subset. Multiline `case WORD in`
branches support exact patterns, `*`/`?`, and simple `|` alternatives through a
Rust-owned matcher; character classes and other POSIX pattern forms remain
outside the profile. Argument-free `break` and `continue` are session control
signals consumed by the innermost Rust-planned loop. Functions also accept
`return [STATUS]`, bounded to 0–255, with an omitted status reusing the
preceding command status. Command substitutions and `sh -c` executions isolate
these control signals from their caller.
`local NAME[=VALUE]` creates a function-scoped binding, capped at 64
declarations per call; the previous value (or absence) is restored when that
function returns, while non-local session changes remain shared.
`shift [COUNT]` updates the active script or function parameter frame without
changing `$0`; it rejects non-numeric or out-of-range counts.
`set -- [ARG ...]` replaces the active bounded positional arguments while
preserving `$0`, with the same 64-argument limit.

The Apple source layer declares command and script App Intents for both the
default and named sessions. They construct a normal `RuneFFISession` rooted at
the app Documents directory and return the Rust result without reimplementing
command behavior in Swift. Named session identifiers are validated in Swift for early feedback and
again in Rust before the persisted namespace is opened. This is a real
automation boundary, but App Intent registration, entitlements, and Shortcuts
runtime execution remain unverified until an Apple target can be built.

Native file automation uses `Session::read_file` and `Session::write_file`, not
shell-string interpolation. Both operations stay inside the session VFS and
enforce a separate 16 MiB transfer limit; writes replace one file and do not
create parent directories. The FFI returns binary reads with an explicit
pointer/length release function, so NUL bytes are preserved. Source-only
Shortcuts currently expose the safer UTF-8 text Put/Get surface and reject
non-UTF-8 reads, while the
binary-safe FFI remains available for a future validated native file type.

The source-only workspace tab layer creates the default session for the first
tab and named Rust sessions for additional tabs. Swift owns tab selection and
presentation; Rust owns each tab's shell state and persistence. Swift persists
only bounded tab metadata in UserDefaults, excluding external paths and Apple
bookmark bytes. The named WindowGroup accepts a typed window route: each
additional iPad window receives its own Rust session namespace, bounded tab
metadata key, and selected-tab key, while the default window preserves the
legacy key.
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
SGR foreground/background colors, 256-color/RGB colors, bold, underline, and
inverse controls after the Rust boundary. It also normalizes carriage returns,
backspaces, and bounded erase-line (`K`) controls for progress-style output.
Parsed spans are cached when an immutable transcript entry is created, so
SwiftUI body recomputation does not rescan raw output. Unsupported terminal
controls are deliberately bounded and not claimed as a complete emulator. This
is source-level performance evidence;
Apple frame-time and device-memory measurements remain unverified.

In parallel, `rune-core` maintains a bounded cursor grid with streaming
CSI/OSC parsing, cursor addressing, bounded scroll regions, and
character/line erasure;
`rune_session_terminal_snapshot` and `rune_session_terminal_cursor` expose
its visible text and zero-based caret position through the C ABI.
The Swift terminal view can switch to a source-only screen surface that draws
that state; the styled line-oriented transcript remains the default until the
native surface is validated on Apple.

The terminal view also declares native keyboard shortcuts for folder import,
cooperative cancellation, history navigation, reverse history search, command
submission, and a source-only settings sheet. The UIKit command editor also
routes bare hardware-keyboard Up/Down presses to the model's history navigation;
the SwiftUI fallback retains the visible history buttons. An optional bounded input toolbar adds
Tab/completion cycling, Escape dismissal, Ctrl-C, display-clear, and paste controls.
Display-clear resets and persists the Rust terminal grid without recording a
shell command. These
controls dispatch into the existing Swift model and Rust FFI rather than
maintaining a second command, configuration, or history implementation; the
toolbar visibility preference is persisted by Rust and surfaced through the
same FFI. Their behavior still needs Apple keyboard/runtime validation.

Clipboard access is another explicit host capability. Rust owns the bounded
`pbcopy`/`pbpaste` command semantics and accepts at most 1 MiB of UTF-8 text;
the CLI has no provider by default. The source-only Apple bridge installs a
UIKit `UIPasteboard` adapter through the C ABI, so clipboard access is not an
ambient capability of the portable core. UIKit privacy prompts and runtime
behavior remain unverified without an Apple build.

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

The current registry includes bounded text filters (`awk`, `cut`, `head`, `tail`,
`grep`, `sed`, `sort`, `uniq`, `wc`, and `diff`). They consume the same in-memory pipeline stdin
as file commands, accept `-` as an explicit stdin operand, and never delegate
to a host shell. `sed` supports one or more bounded regular-expression `s///`
scripts supplied positionally or with `-e`, applied in order, with `g` and `p`
flags plus `-n`; replacements may reference the whole match with `&` or a
capture with `$1`/`\1`. Addresses and complete BSD/POSIX option compatibility
remain outside this profile. These option surfaces are deliberately smaller
than POSIX utilities until compatibility tests justify expanding them.
`grep` compiles bounded Rust regular expressions and supports `-i`, `-v`, `-n`,
`-c`, `-E`, and `-e`; `-F` and `fgrep` use fixed-string matching, while
`egrep` selects the regular-expression mode. Invalid patterns and patterns
above 16 KiB return status 2 before input traversal. Match status remains 0/1
and usage or compilation failures use status 2. This is a bounded regular-
expression surface, not a claim of complete BSD/POSIX option compatibility.
`head` and `tail` share a bounded line-selection parser: `-n`/`--lines` and
short numeric forms select a prefix or suffix, `+N` starts at one-based line N,
and signed `head -n -N` omits the final N lines. `--` ends option parsing.
`sort` supports lexical ordering plus integer-prefix ordering with `-n`,
reverse ordering with `-r`, and adjacent duplicate removal with `-u`; malformed
numeric prefixes are ordered after valid numeric lines.
`wc` counts newline-delimited lines, Unicode words, bytes, Unicode characters,
and maximum line length through the VFS or pipeline stdin. It supports the
bounded `-l`, `-w`, `-c`, `-m`, and `-L` fields, long names, explicit `-`,
multiple-file totals, and `--`; the output order is deterministic rather than
a complete locale-aware formatting implementation.
`diff` reads two confined UTF-8 files, uses a bounded longest-common-subsequence
comparison, emits a whole-file unified view, and returns status 0/1 for equal or
different inputs. Its input, line, and dynamic-programming-cell limits reject
oversized comparisons before the table is allocated.

`awk` is a Rust-owned field-processing subset. It supports one-character or
whitespace `FS`, `OFS`, `$0`/`$1...`, `NF`/`NR`/`FNR`, `print`, `BEGIN`/`END`,
regular-expression line filters, simple equality, and regex-aware `~`/`!~`
predicates. Programs, regexes, UTF-8 input, output, rules, and statements are
bounded; arbitrary awk code remains outside this boundary.

The `find` built-in walks the same virtual filesystem through `metadata` and
`list`; it never traverses the host root directly. Its initial surface supports
one start path, `-name` basename matching, `-type f|d|l`, `-mindepth`, and
`-maxdepth`. Traversal is capped at 10,000 visited entries and does not follow
symlink entries, keeping a
malicious or cyclic tree from turning a synchronous command into unbounded
work.

The portable utility slice adds bounded `base64`, `basename`, `bc`, `cksum`,
`date`, `dirname`, `du`, `expr`, `file`, `md5`, `mktemp`, `realpath`, `rmdir`, `sha256`, `stat`, `sum`,
`tree`, `unlink`, `tee`, `tr`, and `xxd` commands, plus
`ln -s`/`readlink` and
the `unsetenv` spelling for environment removal. They operate on Rune's virtual filesystem or pipeline
stdin only; they do not invoke host executables. `tr` supports literal Unicode
character translation/deletion, while `xxd` supports plain and classic hex
output with a 256 KiB input limit. `base64` uses standard alphabet/padding,
accepts `-d`/`--decode`, and limits input to 768 KiB; decoded bytes must be
valid UTF-8 because the core output boundary is text-based. `cksum` uses the
POSIX CRC-32 polynomial and includes the bounded byte length in its result.
`md5` is implemented locally for legacy digest compatibility and is explicitly
not presented as a secure password or integrity primitive.
`date` exposes only current local/UTC formatting and does not set the host
clock; `sum` implements bounded BSD and System V checksum modes without
invoking a host executable. `bc` uses a Rust-owned bounded integer parser and
does not provide a math library, decimal scale, variables, or arbitrary
precision.
`expr` uses a Rust-owned bounded parser for integer arithmetic, comparisons, and
the `length`, `index`, and `substr` text forms; it rejects overflow, division by
zero, and unsupported expression syntax rather than delegating to a host shell.
`mktemp` uses the VFS's exclusive file-creation capability and platform entropy
to replace bounded `X` runs, retries only collisions, and supports `-d` for
directories. Its insecure name-only `-u` mode is intentionally unavailable.
`file` performs bounded metadata and magic-prefix identification through the
VFS; it intentionally exposes a small Rust-owned classifier instead of linking
or emulating the full `libmagic` database.
`tree` walks only VFS directory listings, caps depth, entries, and rendered
bytes, and does not recurse through symlinks. Its options are deliberately
smaller than the full external `tree` utility.
`cp -r` copies regular-file directory trees
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

`realpath` canonicalizes existing files and directories through the same VFS
boundary, reports the virtual `~` path rather than a host path, and rejects
every resolution outside an approved root. `sha256` hashes stdin or one
bounded VFS file and emits a digest plus operand name; it does not expose host
paths or invoke an external hashing process.

The host-backed VFS can model the standard Apple app layout with Documents as
the virtual home (`~`), plus separately approved `Library` and `tmp` mounts at
`~/Library` and `~/tmp`. The mount roots are canonicalized independently,
reserved root operations are rejected, `..` at a mount boundary returns to the
virtual home, and symlink targets may resolve only inside one of the approved
roots. External-folder sessions use the single selected folder as their root;
they do not gain access to the app's Library or temporary directory.

The shell also provides deterministic `uname` and `whoami` identities for the
portable session. `which` reports aliases and registered built-ins, while
`type` describes aliases, Rust built-ins, installed package commands, and
missing names. These commands never expose the host user's name or claim that
arbitrary host executables are available; installed package commands are
discovered from local package manifests and reported with their package and
version.
`command -v` and `command -V` expose the same discovery boundary for scripts.
The execution form bypasses aliases for one target and dispatches through the
same Rust registry or verified package manifest path; it never probes host
executables.
The registry also exposes `cc`, `c++`, `clang`, `clang++`, and `tex` as
provider-backed entry points. They validate source files and dispatch only to
an explicit Rust `ToolchainProvider`; with the default disabled providers they
return status 126 and never start a host compiler. Generated files can enter
the VFS only as validated relative artifacts from an installed provider.

Native completion asks the Rust session for replacement tokens. At the start
of a line it returns built-in, session-alias, recent-history, and installed-
package command names; for
supported path-oriented
commands it lists only entries in the bounded VFS, preserves virtual prefixes
such as `~/` and `../`, marks directories with `/`, and caps results at eight.
Simple separated `<`/`>` redirection targets use the same confined path list;
simple command/path completion after one `|` uses the same Rust registry and
VFS lookup. Session-local bookmark prefixes such as `~project/` are resolved
through the stored virtual path before directory enumeration. Rust also
validates the selected candidate and returns the complete
replacement command through the C ABI. Quoted, escaped, option, and other
compound-shell fragments are intentionally deferred until the completion grammar
has structured replacement ranges. The Swift model keeps the original
replacement input while cycling
through the bounded candidates on repeated Tab presses, so selecting a second
candidate does not append it to the first. Escape clears that transient editor
state and does not become shell input.

The `apropos` builtin searches only the Rust registry's command names and
summaries, using bounded case-insensitive substring terms. It does not inspect
host manuals, PATH entries, or external processes; a no-match query returns a
visible status 1 so scripts and native clients can distinguish it from usage
failure.

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

The runtime crate exposes a separate `ToolchainProvider` contract for the
longer-term C, C++, and TeX path. A request carries bounded source bytes,
explicit arguments/environment/stdin, and cooperative cancellation. A provider
returns captured diagnostics plus relative, typed artifacts; the caller must
validate those artifacts before materializing them through the VFS. The
default provider is an explicit unavailable implementation. This contract is
not a compiler, linker, TeX engine, or compatibility claim, and no toolchain
payload is installed in the current disk-constrained workspace.
`rune-ffi` exposes an equivalent synchronous callback. Rune owns the stdout,
stderr, path/media metadata, and aggregate artifact buffers for the callback;
the native provider fills lengths and offsets, after which Rust copies and
validates the result. This avoids an outliving-pointer ABI and preserves the
same confined materialization rule. The callback is an integration boundary,
not evidence that a C/C++ compiler or TeX engine exists.

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
repositories and publisher policy remain future work.

The archive command layer implements deliberately narrow stored/Deflate ZIP32 and
USTAR profiles, with optional bounded gzip wrapping for tar. ZIP creation writes
UTF-8 entries and CRC32 values through the VFS;
USTAR creation writes regular-file and directory headers, using the standard
name/prefix fields for longer paths. ZIP and tar filters preserve archive order
and match exact members or descendants of a named directory; ZIP listing uses
`-l`, while filtered extraction uses `-d DESTINATION`. Both recursive traversals are bounded to
10,000 entries and complete archives to 64 MiB. ZIP extraction rejects
encryption, unsupported compression methods, mismatched data descriptors, multi-disk records,
duplicate names, absolute paths, and dot or parent components, then verifies each
local entry's name, bounds, and CRC. USTAR extraction verifies header checksums, rejects
absolute or parent paths, duplicate names, links, device nodes, and unsupported
extensions before writing into the confined destination. The separate gzip
command layer uses the Rust flate2 backend, while `compress`/`uncompress` use a
bounded 9-bit block-mode LZW `.Z` profile. Both are file-to-file transforms that
preserve sources and refuse overwrites or binary stdout. ZIP64, PAX extensions,
and broad external compatibility are not claimed.

The `ar` command layer implements a bounded regular-file archive profile.
Member names are limited to 255 bytes; names longer than the inline 15-byte
field use BSD extended-name records. `ar -rcs` rebuilds or replaces members,
`ar t` lists all or named members, and `ar x` extracts them through the VFS
after preflighting destinations. Member payloads remain binary-safe. Rune
reads common external symbol-index, BSD extended-name, and GNU long-name-table
records but emits only BSD extended names and does not generate or preserve
symbol indexes; directories and linker semantics remain outside the profile.

The `xargs` layer is coordinated by `rune-core` because it must invoke the
existing parser and execution planner for each bounded batch. Its Rust-owned
argument reader supports whitespace or NUL delimiters, a bounded `-n` batch
size, and `-r` empty-input suppression. It quotes generated values before
parsing, so input cannot introduce shell operators or host-process execution.
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
WASI provider, declared `.js` entries run through the bounded JavaScript
provider, declared `.lua` entries run through the bounded Lua provider, and
declared `.rune` entries run through the same Rust parser and script limits as
`source`. Installed module/script bytes are verified again
before execution. `pkg search QUERY` performs a bounded, case-insensitive search
over installed package names, versions, descriptions, and command names. An
explicit `pkg search --registry INDEX_URL QUERY` fetches a versioned HTTPS index
through the session's host network capability. `pkg install --registry INDEX_URL
NAME VERSION` and the corresponding `pkg update` form fetch the exact manifest
and each declared artifact, require HTTPS URLs on the registry origin, verify
the manifest identity and SHA-256 bytes, and only then materialize the package.
Remote update never selects an implicit latest version; a failed fetch,
verification, or write leaves the current package intact. `pkg` remains
network-disabled unless a host provider is installed. Installed WASM commands receive no filesystem
preopen by default. A manifest must explicitly declare
`permissions.filesystem: true` before that command can receive the approved
Rune sandbox as `/`; unknown capability fields are rejected. Network access is
not a package capability.

The registry index is schema version 1 JSON with bounded entries shaped as
`{name, version, description, manifest_url, artifacts:[{path, url}]}`. Its
artifact mapping must exactly cover the downloaded manifest's file list; the
index is discovery metadata and is not treated as a publisher signature.

The runtime contract is owned by Rust and carries only explicit program bytes,
arguments, environment, and stdin into a provider. `rune-wasm` adapts its
bounded WASI result to that contract, while `rune-runtime` provides fresh,
bounded Python, Lua 5.4, and QuickJS states with captured output and explicit
input/resource limits. Python runs without the host standard library and
rejects imports, dynamic code, loops, and function/lambda definitions until a
public per-instruction interrupt budget is available in RustPython.

HTTP is a separate explicit capability rather than an ambient core service.
The Rust `curl` command validates the URL, method, headers, request body, HTTP
failure policy, and output destination before invoking a `NetworkProvider`.
`nslookup` uses the same capability for a bounded, non-interactive
DNS-over-HTTPS request: it validates the DNS name, record type, HTTPS endpoint,
and JSON response, then emits only validated answer data. Neither command opens
sockets in the Rust core. Sessions default to a disabled provider; the Apple
source adapter supplies a bounded synchronous `URLSession` callback that fills
a Rust-owned response buffer. This keeps network and WASM capabilities
separate and leaves ATS, TLS, redirects, and Apple runtime validation as
explicit gates.
The bounded `whois DOMAIN` command uses the same boundary for an HTTPS RDAP
request, validates the domain and endpoint, caps the response at 256 KiB, and
rejects non-UTF-8 or terminal-control output. It does not create a traditional
WHOIS port-43 socket in the portable core.

The `test` and `[` built-ins are Rust-owned expression evaluators rather than
delegation to a host shell. Their bounded grammar covers string and integer
comparisons, `-n`/`-z`, confined VFS existence/type/size predicates, `!`,
parenthesized expressions, and `-a`/`-o` composition. Invalid or sandbox-
escaping path checks return a usage failure instead of being treated as a
successful host lookup.

External application opening is a separate OpenProvider capability. The Rust
openurl command allows only bounded http, https, mailto, tel, sms, and
shortcuts URLs; open additionally resolves an existing regular file or
directory through the confined VFS and passes its canonical approved host path
to the provider. Sessions default to a disabled opener, and the source-only
UIKit adapter schedules the accepted target on the main queue. The callback
never searches for or launches a host executable, and its asynchronous
acceptance does not prove that another application opened successfully.

The native configuration query is also Rust-owned: its FFI serialization now
includes every persisted key consumed by Swift, including cursor shape, while Swift applies only values
from the validated finite sets. This keeps the settings sheet from maintaining
a second configuration source; visual rendering remains an Apple-runtime gate.

Unquoted `*` and `?` are expanded by `rune-core` through the VFS `glob` method;
quoted patterns remain literal, hidden entries require a leading `.`, and an
unmatched pattern remains a literal argument. The VFS validates every matched
candidate against its canonical root before returning it.

Apple source integration is validated without installing a new Xcode or
simulator footprint. The available host Swift toolchain typechecks the native
sources and imported C ABI; iOS SDK compilation, linking, and runtime behavior
remain unverified gates rather than assumed capabilities.

Automation uses the same ownership boundary: `Session::execute_script` runs
non-empty newline-delimited lines through the Rust parser and returns combined
stdout/stderr plus the last status. The input is limited to 256 KiB and 1,024
lines; accepted scripts cap accumulated output per channel after each line.
`rune-ffi` and the Swift source bridge expose that method for a future Apple
Shortcuts adapter; they do not register an Intent or claim Shortcuts runtime
compatibility yet.
