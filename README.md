# Rune

Rune is a Rust-native Unix-like terminal environment for iPhone and iPad, with
a first-class native Apple frontend planned around Swift and SwiftUI. It is an
independent implementation, designed from first principles for iOS sandbox
constraints, reliable command execution, and portable core behavior.

The local `base/a-shell/` checkout is a behavioral and product reference. It is
excluded from Git and no a-Shell source is part of Rune.

## Development Progress

**Overall progress: 88%**

This is an intentionally conservative engineering estimate. The repository
foundation and first Rust shell slice are locally verified; there is not yet a
working iOS application or a feature-parity claim.

| Area | Progress |
|---|---:|
| Rust workspace | 80% |
| Shell tokenizer/parser | 78% |
| Command runtime | 99% |
| Sandboxed filesystem | 70% |
| Archives | 83% |
| Sessions/history | 70% |
| Configuration | 75% |
| WASM | 52% |
| Native iOS UI | 72% |
| Swift/Rust bridge | 64% |
| Package manager | 66% |
| Compatibility evidence | 3% |

## Current status

The first Rust vertical slice is implemented and locally verified. It executes
the supported built-in commands `pwd`, `cd`, `ls`, `cat`, `base64`, `bc`, `cksum`, `curl`, `nslookup`, `whois`, `date`, `echo`, `expr`, `jsc`, `lua`, `python3`, `md5`, `mkdir`,
`touch`, `mktemp`, `rm`, `cp`, `mv`, `env`, `export`, `unset`, `unsetenv`, `printenv`,
`setenv`, `printf`, `basename`, `dirname`, `diff`, `du`, `file`, `realpath`, `rmdir`, `sha256`, `stat`, `sum`, `unlink`, `tee`, `tr`, `tree`, `xxd`,
`alias`, `unalias`, `find`, `sed`, `ln`, `readlink`,
`true`, `false`, `ar`, `awk`, `compress`, `cut`, `head`, `tail`, `grep`, `egrep`, `fgrep`, `gzip`, `gunzip`, `sort`, `uniq`, `uncompress`, `wc`, `wasm`, `pkg`, `tar`, `type`, `command`,
`bookmark`, `showmarks`, `jump`, `renamemark`, `deletemark`, `clear`, `config`,
`help`, `history`, `sleep`, `uname`, `which`, `whoami`, `xargs`, `pbcopy`, `pbpaste`,
`test`, `[`, `source`, `.`, `sh`, `dash`, and `return` against a
bounded filesystem,
including basic `*`/`?` pathname
expansion with quote and hidden-file rules,
with quotes, variables, bounded `$(...)` command substitutions, leading `NAME=value` assignments, pipes, redirections,
word-boundary comments, sequencing, `&&`/`||` short-circuiting, separate
stdout/stderr, and exit status.
`ls` also provides a bounded metadata view with `-l`, human-readable sizes with
`-h`, hidden-entry selection with `-a`/`-A`, and `--` path termination; it only
prints metadata exposed by the VFS and does not invent POSIX permissions or
timestamps.
Assignments are expanded left-to-right, remain session-local, and can also be
issued without a command.
The text pipeline also supports bounded regular-expression `sed` substitutions:
one or more `s///` scripts supplied positionally or with `-e`, applied in order,
with optional `g`/`p` flags and `-n`, plus `&`, `$1`, and `\1` replacement
references. Addresses and full BSD/POSIX compatibility are not implemented yet.
The text pipeline also includes bounded `cut` field/character selection
(`-f`, `-c`, `-d`, and `-s`) over stdin or sandbox files. Its text filters
accept `-` as an explicit stdin path when file operands are present.
`head` and `tail` support bounded line selection with `-n`/`--lines`, the
short numeric form, `+N` from a one-based starting line, and the signed
`head -n -N` form that omits the final N lines; `--` terminates options.
`grep` supports bounded Rust regular expressions with `-i`, `-v`, `-n`, `-c`,
`-E`, and `-e`; `-F` and the `fgrep` alias select literal matching, while
`egrep` selects the regular-expression mode. Invalid patterns and patterns
above 16 KiB fail before input traversal;
its status remains 0 for a match, 1 for no match, and 2 for invalid usage.
`diff [-u|--unified] FILE1 FILE2` compares two bounded UTF-8 VFS files with a
whole-file unified view, returning status 0 when equal, 1 when different, and
2 when input, comparison size, or usage bounds are rejected.
`sort` supports bounded lexical or integer-prefix ordering with `-n`, reverse
ordering with `-r`, and adjacent-result deduplication with `-u`.
`wc` reports bounded line, word, byte, character, and maximum-line-length
counts with `-l`, `-w`, `-c`, `-m`, and `-L`; long option names, explicit stdin
with `-`, multiple confined files, totals, and `--` termination are supported.
`file` identifies bounded VFS entries and stdin using a small Rust-owned magic
surface for directories, empty/text/binary data, ELF, WebAssembly, gzip, ZIP,
and USTAR; `-b` omits the input label and `--mime-type` returns the bounded MIME
classification. It is not a `libmagic` replacement.
`tree` renders a bounded Unicode directory view through the VFS, with hidden
entry selection via `-a`, directory-only output via `-d`, and depth control via
`-L`; it never follows symlink entries.
`find` supports a bounded start path with `-name`, `-type f|d|l`, `-mindepth`,
and `-maxdepth`; it reports symlink entries without following them.
`awk` provides a Rust-owned bounded text-processing subset: `-F` field
separators, `$0`/`$1...`, `NF`/`NR`/`FNR`, `print`, `FS`/`OFS` assignments,
`BEGIN`/`END`, regular-expression `/.../` filters, simple equality, and
regex-aware `~`/`!~` predicates. It accepts UTF-8 stdin or confined files and
does not execute arbitrary awk code.
Command substitutions execute through the same Rust planner with a bounded
nesting depth; their stdout loses trailing newlines, shell state is isolated,
and filesystem writes remain real VFS writes.
Redirections also support bounded stream duplication with `2>&1`, `1>&2`,
`>&2`, and `&>`/`&>>`; the Rust plan preserves their left-to-right target
semantics for pipelines and captured stdout/stderr.
`base64` encodes stdin or one confined file and decodes strict standard Base64
back to UTF-8, with a 768 KiB input bound and explicit errors for malformed or
non-text decoded data.
`date` prints the current local or UTC time using a bounded Rust-owned format
surface; clock-setting and platform-specific date parsing are intentionally not
exposed. `cksum` computes the POSIX CRC checksum for one bounded VFS file or stdin,
including the input length in its stable two-field output.
`sum` computes bounded BSD-style checksums by default and the bounded System V
variant with `-s`; it is a compatibility utility, not a cryptographic digest.
`bc` evaluates bounded integer expressions from stdin or one confined file,
including parentheses, unary signs, arithmetic operators, comments, and
statement separators. The standard math library, decimal scale, variables,
and arbitrary precision remain outside this subset.
`md5` computes the standard MD5 digest for one bounded VFS file or stdin for
legacy compatibility workflows; it is not a security primitive.
`expr` evaluates bounded integer arithmetic and comparisons, plus `length`,
`index`, and `substr` text operations; regular-expression expressions and
floating-point arithmetic remain outside this subset.
A synchronous command response and each intermediate pipeline channel are
capped at 1 MiB per output channel; a truncation marker is emitted rather
than allowing unbounded terminal output.
Individual command lines are capped at 64 KiB before parsing, and automation
scripts have their separate 256 KiB/1,024-line input boundary.
`source FILE [ARG ...]` and `. FILE [ARG ...]` execute bounded UTF-8 script files
through the same Rust parser, session environment, VFS, status, and history
path. Scripts receive bounded positional values as `$0`, `$1...`, `$#`, and
`$@`; stdin from an enclosing pipeline is preserved for the script's first
command. Nested sourcing is capped at 16 levels and accepts at most 64
arguments.
`sh -c SCRIPT` and `dash -c SCRIPT` execute an inline bounded script through
that same Rust planner, with an optional `$0` name and up to 64 positional
arguments; they never start a host shell.
Multiline automation scripts also support bounded `for NAME in VALUE ...; do`
loops, including nested loops; each loop accepts at most 256 expanded values
and keeps the loop variable in the Rust session. They also support multiline
`if/elif/else/fi` branches whose conditions run through the same Rust planner;
control-flow nesting is capped at 16 levels. Multiline `case WORD in` branches
support exact patterns, `*`/`?`, and simple `|` alternatives. Multiline
`NAME() { ... }` function definitions support bounded positional arguments and
shared session state; at most 256 functions, 64 arguments per call, and 16
recursive calls are allowed. POSIX character classes and one-line function or
control-flow bodies remain outside this subset. `while` and `until` stop after
at most 1,024 body iterations and
return a bounded status-2 error if the limit is reached. Loop bodies can use
argument-free `break` and `continue`; they are rejected outside a Rust-planned
loop and do not leak through `sh -c` command substitutions. Functions can use
`return [STATUS]` to stop their current body; the status is limited to 0–255,
and an omitted status reuses the preceding command status.
`type` complements `which` by describing aliases, Rust built-ins, installed
package commands, and missing names without exposing host executables.
`command -v` and `command -V` provide the same bounded discovery for scripts;
the execution form (`command PROGRAM [ARG ...]`) bypasses aliases and dispatches
through Rune's Rust registry or verified package manifests, without probing
host executables.
The Rust-owned `test` and `[` built-ins evaluate bounded file predicates
(`-e`, `-f`, `-d`, `-L`, `-h`, `-s`), string predicates, integer comparisons,
negation, and `-a`/`-o` composition so scripts can branch without a host shell.
The Rust session and C/Swift bridge also expose cooperative cancellation at
command, pipeline, script, and bounded traversal boundaries, returning status
130; `sleep` polls that same cancellation flag in bounded 25 ms intervals, while
an unrelated synchronous operation is allowed to finish.
The event-aware Rust/FFI execution path delivers bounded output after each
completed pipeline and status/directory events at command boundaries. Swift
copies those borrowed callback strings and renders them through the same
bounded transcript policy; this is boundary-level event delivery, not live UI
rendering or byte-level WASM streaming.
Environment changes are not serialized by default. On restore, Rune selects a
bounded startup profile in this order: `~/.rune_profile`, `~/.profile`, then
`~/.bashrc`. Its supported Rust built-ins can update the session environment
and define aliases, with output surfaced to the CLI/native boundary without
polluting history. Alias expansion is bounded and currently accepts one
command per alias value; compound alias values are rejected explicitly. The
native source UI now asks Rust for bounded command and sandbox-path completion;
the registry and filesystem lookup remain Rust-owned and the bridge exposes only
replacement tokens. Simple separated `<`/`>` redirection targets use the same
confined path completion; quoted, escaped, option, and compound-shell fragments
remain deferred.
It also has a focused command bar, keyboard-aware history controls, an
ink/cyan/ember console palette, accessible completion controls, native handling
for the Rust `clear` screen-control sequence, and source-only ANSI rendering
for common SGR foreground/background colors, 256-color/RGB colors, bold,
underline, and inverse output.
Interactive `export`, `setenv`, and assignment lines are replaced by a
redaction marker in history before persistence when the Rust-owned
`history-redaction` setting is enabled. It is enabled by default and remains a
narrow detector, not a complete secret management policy; setting it to false
is an explicit opt-out that may persist command secrets.
The iOS app is represented by
source-only SwiftUI and FFI boundaries, but its Apple compilation, linking,
and runtime gates remain unverified. Bounded directory enumeration and
current-directory/history persistence now exist in Rust; bounded
`history-limit`, `history-redaction`, `environment-persistence`, `font`, `font-size`, `scrollback-limit`, `toolbar-visible`, `theme`,
`cursor-color`, `cursor-shape`, `background`, and `foreground` configuration
is available, and the native source UI consumes the font size, bounded
scrollback window, three named palettes, the Rust-owned font design, cursor color,
and independent background/foreground overrides. The native source-only command
editor applies the configured bar, block, or underline caret when UIKit is
available. Environment restoration is opt-in: `environment-persistence` is
false by default, and enabling it persists only bounded user-defined variables;
`HOME`, `PATH`, `RUNE_VERSION`, `TERM`, `PWD`, and `OLDPWD` are never restored
from session state. The explicit opt-in can still store exported values, so it
must not be enabled for secrets. Broader terminal-state and scene recovery
remain planned.
The bounded Python, Lua 5.4, and JavaScript providers are implemented in Rust.
Python supports sandbox files plus `python3 -c CODE` and `python3 -` stdin
entry points. It intentionally starts with a finite, tested subset: host
imports and dynamic code are denied, and loops/functions are rejected until a
public instruction budget is available in the embedded VM.

The Rust package boundary now validates bounded, versioned JSON manifests and
registry indexes, and checks declared file bytes with SHA-256. Local installation
and update are available without network access. An explicit HTTPS registry can
also serve bounded search results and exact-version package installation/update
through the injected host network capability; the downloaded manifest and every
artifact must match, stay on the registry origin, and pass digest verification.
There is no implicit `latest` selection, publisher signature system, or ambient
network access.

The `wasm MODULE [arg ...]` built-in loads a module through the bounded virtual
filesystem and executes WASI preview1 `_start` in Rust. It exposes
stdin/stdout/stderr, arguments, and the session environment, plus explicit
WASI preopens mapped to Rune's approved sandbox roots. Capability-based
opening keeps guest filesystem calls inside those roots; arguments, environment
entries, and stdin are bounded before WASI setup. No host process or network
capability is inherited. Session executions consume cancellation at runtime
boundaries and return status 130 when it is observed. A WASM call may
finish before a cancellation callback is observed. Module bytes,
interpreter fuel, linear memory, tables, and captured output are bounded.
For the three-root Apple layout, WASI receives `/` for Documents/home and
explicit `/Library` and `/tmp` preopens; single-root and external-folder
sessions receive only `/`. No arbitrary guest preopen is inherited.
ZIP64 and broader WASI resource policy, and the broader Python stdlib/package
surface remain planned; `.gz` and `.Z` file transforms are
implemented as separate bounded Rust command layers.

The package flow supports `pkg info MANIFEST|NAME [VERSION]`, `pkg verify
MANIFEST`, `pkg install MANIFEST`, `pkg update MANIFEST`, `pkg list`, `pkg search
QUERY`, `pkg search --registry INDEX_URL QUERY`, `pkg install --registry INDEX_URL
NAME VERSION`, `pkg update --registry INDEX_URL NAME VERSION`, and `pkg remove NAME
[VERSION]`. Once installed, `pkg info NAME` resolves the sole
installed version; an explicit version is required when multiple versions are
present. Install
copies only SHA-256-verified files into `~/.rune/packages`; declared `.wasm`
commands, `.py`, `.js` and `.lua` scripts, and `.rune` scripts can then run through Rust, and `which` discovers
their installed command names from the local package manifests. Installed
WASM commands receive no filesystem preopen by default; a manifest must
explicitly declare `permissions.filesystem: true` to request the approved Rune
sandbox as `/`. `pkg update MANIFEST` verifies and materializes a different
local version before retiring the currently installed version; a failed
verification or write keeps the old version. Remote updates require an explicit
registry URL and target version; a failed fetch, identity check, digest check,
or materialization keeps the old version. The `--remote` flag is accepted as a
compatibility alias for `--registry`.

The bounded `curl` command owns HTTP request parsing in Rust and accepts GET,
HEAD, POST, PUT, and DELETE through explicit method selection, bounded headers,
bounded text data, HTTP failure handling, and raw response output to a confined
VFS file. The Rust CLI has no network grant by default. The source-only Apple
bridge supplies a synchronous, size-limited `URLSession` callback for an
eventual native target; URLSession, ATS, transport, and device behavior remain
unverified without an Apple runtime. `nslookup HOST` adds a deliberately
non-interactive DNS-over-HTTPS subset with bounded A, AAAA, CAA, CNAME, MX, NS,
PTR, SOA, SRV, and TXT queries. `--server` selects an explicit HTTPS DoH
endpoint, and the resolver response is reduced to validated answer data; the
core never opens DNS sockets or exposes resolver JSON in command output.
`whois DOMAIN` uses the same explicit network capability with a bounded HTTPS
RDAP request and safe UTF-8 text output. It is intentionally not a port-43
WHOIS socket implementation; live RDAP routing and server behavior remain
runtime/provider gates.
The open and openurl commands validate approved URL schemes or existing
confined VFS files/directories, then call an explicit host-open capability.
The default CLI has no launcher, and the source-only Apple adapter schedules
UIKit opening on the main queue; it does not expose arbitrary host paths or
claim completion before Apple runtime validation.

The Rust core also provides bounded `ar -rcs`/`ar t`/`ar x` member archives,
zip -r ARCHIVE FILE ..., unzip ARCHIVE [DESTINATION], unzip -l ARCHIVE
[MEMBER ...], or unzip ARCHIVE -d DESTINATION [MEMBER ...], tar -cf/-tf/-xf ARCHIVE,
gzip FILE ..., gunzip FILE.gz ..., compress FILE ..., and uncompress FILE.Z ...
commands. `ar` stores regular files with bounded member names up to 255 bytes,
encodes longer names with BSD extended records, and reads common external
symbol-index and GNU long-name records without emitting symbol tables. ZIP uses
stored or Deflate ZIP32 entries through the VFS, verifies
CRC32, accepts validated data descriptors before extraction, and supports
bounded member listing with `-l` plus member filters with `-d`. Tar uses UTF-8
USTAR entries with long names split across the standard name/prefix fields;
`tar -z` adds bounded gzip compression for create, list, and extract flows.
Tar list/extract also accept exact member filters, including directory
prefixes, and reject missing filters without writing partial output.
Gzip and `.Z` LZW are file-to-file Rust backends: they keep the source, refuse
binary stdin/stdout mode and refuse to overwrite destinations, and cap both
input and decompressed output at 64 MiB. These commands reject absolute or
parent-traversal archive names, links, unsupported entry types, and archives
above 64 MiB or 10,000 entries. ZIP64, PAX extensions, encrypted archives, and
compatibility with every external producer remain unsupported until separately
tested.

The bounded `xargs` command consumes whitespace- or NUL-delimited stdin and
invokes the normal Rust command planner in batches. `-n` limits each batch,
`-r` skips an empty input, and generated arguments are quoted before parsing;
there is no shell interpolation or host-process execution.

The host-backed VFS also rejects regular-file reads, appends, and copies over
64 MiB before allocating or copying their contents. This limit is independent
of the smaller 16 MiB native file-transfer boundary and the 1 MiB terminal
output boundary. Directory listing and wildcard enumeration are capped at
10,000 entries to keep large trees bounded before terminal rendering.
`mktemp` creates files with an exclusive VFS operation, replaces a bounded
`X` template using the platform entropy source, retries collisions, and can
create directories with `-d`; the insecure name-only `-u` mode is rejected.

The default Apple session mounts the platform's Documents directory as `~` and
passes the app's Library and temporary directories as confined `~/Library` and
`~/tmp` roots. Relative navigation across a mounted root returns to `~`, root
operations are rejected, and symlink validation accepts only the explicitly
approved three roots. A user-selected external folder intentionally uses a
single root instead of borrowing the app's sibling directories.

The portable core also supports session-local virtual directory bookmarks with
`bookmark`, `showmarks`, `jump`, `cd ~NAME`, `renamemark`, and `deletemark`.
They persist with the session state and remain confined to the configured VFS;
the source-only Apple layer also handles user-selected external folders through
bounded security-scoped bookmarks. Full entitlement, picker, and device/runtime
behavior remain unverified without the Apple toolchain.
Bookmark names are limited to 64 characters, a session holds at most 256
bookmarks, and serialized bookmark data is limited to 256 KiB.

Named Rust sessions keep their virtual working directory, history, and
bookmarks independent under `~/.rune/sessions/{id}/session.state`. When the
explicit `environment-persistence` setting is enabled, bounded user-defined
environment entries are stored in the same namespace. IDs are
validated as bounded opaque names (up to 64 ASCII characters from `A-Z`,
`a-z`, `0-9`, `_`, `-`, and `.`); they are never interpreted as shell paths.
The source-only Swift workspace uses this FFI boundary for independent
terminal tabs. Rust persists each named session's state, while Swift persists
only bounded tab metadata and does not store external paths or bookmark bytes;
the named WindowGroup routes additional windows to independent Rust session
namespaces and separate tab metadata. SwiftUI rendering, scene restoration,
iPad runtime behavior, and Apple runtime behavior remain unverified without an
Apple build toolchain.

Directory changes update the Rust-owned `PWD` and `OLDPWD` values. `cd -`
returns to the previous directory and prints the resulting virtual path, while
bookmark jumps and aliases that change directories use the same state update.

The FFI and Swift source boundary exposes bounded command and newline-delimited
script execution for Shortcuts, including named persisted session namespaces.
Source-only App Intent declarations call that real Rust-backed API and return stdout/stderr/status as text; Rust rejects
scripts larger than 256 KiB or 1,024 lines and caps accumulated output per
channel. App Intent registration, entitlements, and runtime behavior remain
unverified without an Apple build/runtime.

The same boundary exposes bounded binary file transfer through the confined VFS:
`put` replaces one file and `get` returns an explicitly freed byte buffer, with
a 16 MiB payload limit and no implicit parent-directory creation. The
source-only Shortcuts layer provides UTF-8 text Put/Get actions on top of that
real API; arbitrary binary automation remains an FFI capability until an
Apple-native file parameter contract is validated.

The portable core also exposes `pbcopy` and `pbpaste` through an explicit
bounded text clipboard capability. The CLI keeps this capability disabled by
default, while the source-only Apple bridge maps it to `UIPasteboard`; payloads
are limited to 1 MiB and clipboard access occurs only when one of those
commands is invoked.

The source-only terminal now dispatches command execution away from the SwiftUI
main actor behind a lock-protected FFI session, keeps the UI responsive, and
offers a stop control that sends Rust's cooperative cancellation request. Its
optional input toolbar routes Tab/completion, Escape, Ctrl-C, display-clear,
and paste actions through the native model; only command execution and shell
state cross the Rust boundary. A currently running synchronous Rust operation
may still finish before its next boundary; background execution, cancellation,
and Apple runtime behavior are not device-validated here.

The SwiftUI terminal source also provides VoiceOver labels, values, hints, and
stable accessibility identifiers for the session status, transcript entries,
command input, execution controls, completion actions, settings, and workspace
tabs. This is source-level accessibility structure; VoiceOver traversal,
Dynamic Type, contrast, and iPad interaction still require Apple runtime
validation.

The Swift transcript also applies a separate bounded scrollback window
(4,096 entries by default, configurable up to 8,192) and an 8 MiB in-memory
byte cap, dropping the oldest rendered events when either limit is exceeded.
This protects the UI from unbounded replay growth while Rust retains its own
bounded per-command output and persisted history policies.

The portable configuration boundary currently supports bounded `history-limit`,
`environment-persistence`,
`font`, `font-size`, `scrollback-limit`, `toolbar-visible`, `theme`,
`cursor-color`, `cursor-shape`, `background`, and `foreground` settings through `config get`,
`config set`, and `config reset`. The scrollback setting accepts 128–8,192
rendered entries and remains subject to the UI's 8 MiB byte cap. It persists in
`~/.rune/config.state`. Environment persistence is disabled by default and
excludes the core `HOME`, `PATH`, `RUNE_VERSION`, `TERM`, `PWD`, and `OLDPWD`
variables; turning it on is an explicit choice that may store exported values.
The FFI also exposes validated Rust-native set/reset
calls that do not create history entries; the source-only SwiftUI settings
sheet uses those calls for font, font size, scrollback, theme, cursor color,
cursor shape, background, foreground, reset, and toolbar visibility. A separate source-only input toolbar provides bounded
Tab/completion, Escape, Ctrl-C, display-clear, and paste controls; its
visibility, cursor color, and cursor shape are persisted by the Rust configuration
boundary. The UIKit caret implementation is source-only evidence; Apple
compilation and runtime rendering remain unverified.

The `history` built-in also supports `history N` for a bounded recent view,
`history search QUERY ...` for a case-insensitive substring search that keeps
original history numbers, and `history -c` to clear the current session
history. The native SwiftUI command bar adds a Rust-backed reverse-search panel
that returns newest-first matches without recording a synthetic search command.
History records are limited by the configured count and a 4 MiB total
serialized-history budget, so a large count cannot create unbounded session
state. Consecutive duplicate entries are suppressed, while the same command
after another command remains a distinct history record.

Runtime providers use a small Rust-owned request/output contract. WASM, the
bounded Python subset, the bounded Lua 5.4 provider, and the bounded JavaScript
provider are implemented in Rust. Python package imports and the Apple runtime
boundary remain unverified.
The Rust runtime crate also defines a bounded `ToolchainProvider` contract for
C, C++, and TeX: source input, arguments, environment, stdin, cancellation,
captured diagnostics, and relative generated artifacts are validated before
materialization. The default providers are explicitly unavailable; no clang,
C++ compiler, TeX engine, or large toolchain payload is installed or claimed
yet. The `cc`, `c++`, `clang`, `clang++`, and `tex` entry points therefore
return an explicit provider-unavailable status until a real provider is
installed and reviewed; they are not counted as supported compilation or
document-rendering commands above.
The C/Swift boundary now exposes the same contract through a synchronous
callback with Rune-owned output buffers and an aggregate artifact arena; a
native provider can be added later without returning unmanaged pointers or
writing arbitrary host paths. This is ABI/source evidence only and does not
make a compiler or TeX engine available.

No a-Shell compatibility area is marked `supported` without behavior and test
evidence. See [`compat/a-shell-compatibility.json`](compat/a-shell-compatibility.json).
The versioned scenarios in [`compat/scenarios/core.json`](compat/scenarios/core.json)
define a small, bounded differential surface for quoting, pipelines,
substitution, conditionals, confined files, status, and aliases. The local
runner executes each scenario in a fresh temporary Rune filesystem and can
compare separately captured a-Shell observations; it never executes the
reference checkout and keeps direct comparison pending until Apple-runtime
evidence exists.

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

For local timing samples of the warm CLI startup, a confined filesystem
pipeline, and bounded tree rendering, run `./scripts/bench.sh`. It reports
`real`, `user`, and `sys` durations for the current machine; these samples are
engineering measurements, not release or device-performance claims.

Validate the compatibility scenario document with
`python3 scripts/compatibility_runner.py --validate-only`. After
`cargo build -p rune-cli`, run the isolated Rune side with
`python3 scripts/compatibility_runner.py`; provide a directory of separately
captured a-Shell observations with `--reference-dir` to perform a bounded
comparison. A green scenario run alone is not a direct a-Shell compatibility
result.

## Roadmap

### Phase 1 — Foundation

- [x] Repository and Rust workspace
- [x] Local-only quality workflow
- [x] First command execution slice
- [x] Source-only SwiftUI iOS frontend boundary
- [x] Source-only C/Swift bridge
- [ ] Apple target and runtime validation

### Phase 2 — Core shell

- [x] Tokenizer and parser
- [x] Environment and path expansion
- [x] Bounded virtual filesystem
- [x] Built-in file commands
- [x] Bounded session/history persistence and startup profile
- [x] Rust-owned reverse history search through the native bridge
- [x] Pipes and redirections
- [x] Basic bounded pathname expansion
- [x] Leading environment assignments
- [x] Bounded session-local command aliases
- [x] Bounded script-file sourcing with positional arguments and nested execution limits
- [x] Bounded `sh -c`/`dash -c` inline scripts with positional arguments
- [x] Bounded multiline `for` loops in Rust-planned scripts
- [x] Bounded multiline `if`/`elif`/`else` branches in Rust-planned scripts
- [x] Bounded multiline `while`/`until` loops in Rust-planned scripts
- [x] Bounded multiline `case` branches in Rust-planned scripts
- [x] Bounded multiline shell function definitions and calls in Rust-planned scripts
- [x] Bounded function `return [STATUS]` in Rust-planned scripts
- [x] Bounded `break`/`continue` loop controls in Rust-planned scripts
- [x] Cooperative command cancellation boundary, including cancellable `sleep`
- [x] `&&` and `||` conditional chaining
- [x] Bounded recursive `find` traversal with type/depth filters
- [x] Bounded regular-expression `sed` substitutions
- [x] Bounded terminal output channels
- [x] Session-local virtual directory bookmarks
- [x] Bounded portable utility commands
- [x] Bounded long-format and human-readable `ls` metadata
- [x] Bounded virtual filesystem metadata and usage commands
- [x] Confined canonical-path and SHA-256 utility commands
- [x] Bounded Base64 encode/decode utility
- [x] Bounded POSIX `cksum` utility
- [x] Bounded MD5 compatibility utility
- [x] Bounded exclusive `mktemp` file/directory creation
- [x] Bounded `expr` arithmetic and text utility
- [x] Bounded `pbcopy`/`pbpaste` through an explicit host clipboard capability
- [x] Bounded UTF-8 `diff` with unified output and comparison limits
- [x] Bounded Rust-owned `awk` field processing subset
- [x] Bounded Rust-owned `xargs` batching over stdin
- [x] Bounded `$(...)` command substitution with isolated shell state
- [x] Bounded regular-expression and fixed-string `grep` modes
- [x] Bounded numeric, reverse, and unique `sort` options
- [x] Bounded `type` command discovery
- [x] Bounded `command -v`/`-V` discovery for scripts
- [x] Bounded Rust-owned `file` identification utility
- [x] Bounded VFS `tree` directory rendering
- [x] Bounded `test` and `[` predicates for script conditionals

### Phase 3 — Developer environment

- [x] Bounded WASI preview1 runtime boundary and resource limits
- [x] Bounded package metadata, integrity, local WASM installation, and local update
- [x] Portable runtime request/output contract
- [x] Bounded Rust-owned history, history-redaction, opt-in environment persistence, font, font-size, scrollback, toolbar-visible, theme, cursor-color, cursor-shape, background, and foreground configuration
- [x] Bounded `ar` member archives plus stored/Deflate ZIP, USTAR tar, and gzip/.Z file transforms
- [x] Explicit host HTTP capability and bounded `curl` transport boundary
- [x] Bounded non-interactive `nslookup` through an explicit HTTPS DoH provider
- [x] Bounded HTTPS RDAP `whois` query through the explicit network provider
- [x] Bounded HTTPS registry index, remote search, and explicit-version update policy
- [x] Bounded Python subset runtime evaluation
- [x] Bounded JavaScript runtime evaluation
- [x] Bounded Lua 5.4 runtime evaluation
- [x] Bounded C/C++/TeX toolchain provider contract (providers still planned)
- [x] Rust-owned command/path completion and help metadata
- [x] Versioned isolated differential scenarios (direct a-Shell comparison pending)

### Phase 4 — Apple integration

- [x] Source-level lazy terminal transcript with cached ANSI spans
- [x] Source-only external folders and bounded security-scoped bookmarks
- [x] Explicit host URL/file opening capability with bounded open/openurl
- [x] Rust-namespaced sessions and source-only terminal tabs
- [x] Source-only typed iPad window routing
- [x] Source-only native keyboard shortcuts
- [ ] iPad multi-window behavior
- [x] Source-only command/script/file and named-session App Intent declarations
- [x] Source-only settings sheet and bounded input toolbar
- [ ] Apple Shortcuts registration and runtime validation
- [ ] Accessibility and VoiceOver validation

## Non-goals for the current milestone

- claiming a-Shell parity
- copying a-Shell implementation or UI
- executing arbitrary host processes from the shell
- pretending an iOS app exists before it is built and tested
