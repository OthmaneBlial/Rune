# Rune development

## Prerequisites

- Rust toolchain with `cargo`, `rustfmt`, and Clippy
- Existing Apple/Swift tooling is optional for source inspection; do not install
  Xcode, simulators, SDKs, or additional dependencies just for this phase.

The portable WASM slice is validated through Rust tests and does not require
Xcode, a simulator, an Apple SDK, or a third-party Apple dependency.
WASM tests also verify explicit preopens can be supplied and observed while
the default runner remains filesystem-free; each preopen is limited to Rune's
explicitly approved roots. Invocation arguments, environment, and stdin are
bounded before WASI setup. A pre-cancelled module is rejected with status 130;
in-flight WASM cancellation remains cooperative because the current Wasmi call
does not expose a safe mid-stack interrupt API.
Package metadata and local WASM installation tests remain local-only; no
internet access is needed to validate the manifest, digest, and bounded
package-tree boundaries. Manifest capabilities are explicit: installed WASM
has no filesystem preopen unless `permissions.filesystem: true` is declared;
tests cover both the denied-by-default and granted paths.
The package search test is local-only as well and searches installed
manifests without network access. The package update test verifies that an
invalid replacement leaves the old version runnable, while a verified local
replacement is materialized before the old version is retired. Separate
injected-provider tests exercise an HTTPS registry index, case-insensitive
remote search, exact-version remote install/update, same-origin enforcement,
manifest/artifact identity checks, and SHA-256 verification; they do not claim
internet, TLS, redirect, publisher-signature, or Apple URLSession proof.
Package command tests also cover a SHA-256-verified `.rune` script, bounded
positional arguments, and rejection after installed content is tampered with.
Lua runtime tests cover captured stdout/stderr, explicit `arg`/environment/stdin
inputs, disabled unsafe libraries, UTF-8/source-size validation, memory, and
instruction limits. The embedded provider intentionally has no host filesystem,
process, module-loader, or network bridge yet.
JavaScript runtime tests cover `jsc`, captured `print`/`console`/stream output,
explicit `process.argv`/environment/stdin inputs, disabled host modules,
QuickJS memory/stack/instruction bounds, and source/input validation. Its
`process` object is a small Rune-owned data bridge, not Node.js, and no module
loader, host filesystem, process, or network bridge is enabled.
Python runtime tests cover `python3`, captured `print`/stream output, explicit
argv/environment/stdin inputs, denied host file access, source/input bounds,
and package `.py` execution with installed-file integrity verification. The
provider uses RustPython without its host standard library; imports, dynamic
code, loops, and function/lambda definitions are rejected because RustPython
0.4 does not expose a public per-instruction interrupt hook. This is a bounded
Python subset, not CPython or a package/stdlib compatibility claim.
Configuration tests are local-only as well and verify bounded history, font,
font size, scrollback, toolbar visibility, theme, cursor-color, cursor-shape,
background, and foreground settings without requiring an Apple
runtime. The scrollback setting accepts 128–8,192 rendered entries and remains
subject to the native UI's separate 8 MiB byte cap. The `sleep` command test
also triggers cancellation during an in-flight bounded wait, not only before a
command begins.
Clipboard tests also cover the disabled default provider, a real pipe through
`pbcopy`/`pbpaste`, callback installation/removal at the FFI boundary, and the
1 MiB UTF-8 payload limit.
The `diff` tests cover equal and different confined files, unified output,
missing files, invalid UTF-8, and rejected options with distinct statuses.
Base64 tests cover stdin encode/decode, padded input, malformed characters,
non-UTF-8 decoded output, usage errors, and the 768 KiB input bound.
Checksum tests cover the POSIX CRC vector and invalid operand counts. The
portable utility tests also cover UTC `date` formatting and BSD/System V `sum`
vectors, plus arithmetic, comments, separators, overflow, and division errors
for the bounded `bc` surface. Date setting and locale-specific formatting remain
outside the bounded surface.
MD5 tests cover standard stdin/file vectors and invalid operand counts; the
digest is compatibility evidence, not security evidence.
`expr` tests cover precedence, comparisons, bounded text operations, division
by zero, and missing operands; this is a tested subset, not POSIX expression
parity.
The `awk` test covers stdin field selection, `-F`/`OFS`, `BEGIN`, equality
filtering, `END`, and rejection of unsupported actions; it does not establish
full awk language or regular-expression compatibility.
Redirection tests cover append writes, `2>&1`, `1>&2`, `&>`, left-to-right
duplication order, and carrying merged stderr through a pipeline.
Script tests also cover bounded positional arguments (`$0`, `$1...`, `$#`, and
`$@`), preservation of pipeline stdin, and restoration of outer parameters
after nested source calls. The inline `sh -c`/`dash -c` tests cover the same
Rust planner, positional values, pipeline stdin, sequencing, and rejection of
unsupported shell modes without starting a host process.
Shell parser tests cover quoted and nested `$(...)` forms and unclosed
substitution rejection. The core integration test covers stdout capture,
trailing-newline removal, nested substitutions, redirection targets, and
restoration of cwd/environment state; substitution depth and input size remain
bounded.
Completion tests also cover simple separated redirection targets, use only the
confined VFS, and do not require an Apple runtime.
Named-session tests use separate temporary VFS state namespaces and verify that
working directories and history do not leak between Rust or FFI handles.
History-search tests verify newest-first, case-insensitive matches, query bounds,
and the absence of history mutation; the FFI test covers the owned string
returned to the source-only native bridge.
File-transfer tests verify confined paths, the 16 MiB boundary, and binary
payloads containing NUL bytes across the Rust/FFI boundary.
Event tests verify Rust pipeline/status emission and the C callback lifetime:
event strings are borrowed only during the callback and are copied by the
native bridge. This validates boundary-level event delivery, not Apple runtime,
live UI rendering, or byte-level WASM streaming.
The Swift transcript stores parsed ANSI spans once per immutable transcript
entry and renders bounded entries through `LazyVStack`; this is source-level
rendering architecture evidence, not a measured Apple frame-time or device
memory result.
The archive integration tests create nested files, list and extract a USTAR
archive into a new confined destination, and reject compressed tar flags and
escaping archive members. ZIP coverage remains stored-only and does not
establish compatibility with compressed, ZIP64, PAX, or encrypted archives.
The gzip integration test verifies bounded file-to-file compression and
decompression, source preservation, refusal of binary `-c` mode, and suffix
validation. The LZW integration test also crosses a dictionary reset and
verifies `.Z` round-trip bytes. These tests do not establish full gzip or
`compress` command-line compatibility.
The `ar` integration test verifies binary-safe regular-file members, `-rcs`
creation, listing, extraction, and refusal to overwrite existing members;
symbol-table, long-name-table, and linker compatibility remain unverified.
The `xargs` tests verify bounded whitespace and NUL splitting, batch sizing,
empty-input suppression, safe quoting, and execution through the ordinary Rust
command planner; full platform-specific xargs compatibility remains unverified.
The text pipeline tests also verify bounded regular-expression grep, `-e`,
fixed-string `fgrep`, invalid-pattern rejection, `sort -n`, combined `-nru`
flags, and deduplicated numeric output; field keys, locale collation, and full
POSIX sort/grep compatibility remain outside the bounded profile.
The text pipeline tests also cover regular-expression `sed` substitutions,
capture replacement, and invalid-pattern rejection; addresses and multiple
scripts remain outside the bounded profile.
The filesystem resource test uses a sparse file to verify the 64 MiB read,
append, and copy guards without allocating a large in-memory fixture.
The directory resource test creates 10,001 small entries and verifies that
both listing and wildcard enumeration stop at the 10,000-entry boundary.
The filesystem and FFI layout tests also verify that Documents, Library, and
tmp can be mounted as `~`, `~/Library`, and `~/tmp`, with navigation and file
writes staying in their respective approved roots.
The source-only Apple check also parses the external-folder bookmark layer;
that syntax check does not prove security-scoped access, entitlements, or Files
picker behavior on a device.
It also parses the source-only App Intent declarations; the AppIntents module,
registration, entitlements, and Shortcuts runtime remain unverified here.
The same parse check covers the source-only workspace tab container; it does
not prove SwiftUI rendering, tab lifecycle, iPad multi-window behavior, or
Apple runtime integration.
The source-only UI also contains background-task and cancellation code, but
there is no Apple concurrency, device, or runtime validation in this workspace.
Its transcript cache has explicit entry and byte bounds in source, but that
presentation policy cannot be runtime-tested without SwiftUI. The terminal
and workspace sources also expose explicit accessibility labels, hints, values,
and identifiers; VoiceOver traversal and Dynamic Type remain runtime gates.
The source-only renderer also consumes bounded ANSI SGR foreground/background
colors, 256-color/RGB colors, bold, underline, and inverse controls; Apple
text-layout and terminal-control fidelity remain unverified without the Apple
runtime.
The network tests use an injected provider and verify method, URL, headers,
request data, response output, raw VFS-file output, disabled-provider behavior,
and the C callback boundary. They do not prove internet reachability, TLS,
ATS, redirects, or Apple URLSession behavior. The native callback enforces the
same 8 MiB response buffer while receiving data in bounded chunks.

## Local checks

Run `./scripts/ci.sh` from the repository root. It runs formatting, Clippy,
workspace unit and integration tests, and a workspace build. The project
intentionally does not use GitHub Actions or cloud CI.

Run `./scripts/demo.sh` for a real two-process CLI workflow covering file
creation, recursive copy, configuration persistence, restore, metadata, usage,
and history. It uses a temporary root and removes only that root on exit.

## Commit boundaries

Keep commits small enough to describe one coherent behavior. Before a push:

1. inspect `git diff` and `git status`
2. verify `base/` is absent from the staged diff
3. check for secrets and local credentials
4. run the relevant local checks
5. update documentation and the progress estimate when scope changes
6. commit and push directly to `main`

The reference implementation is never staged. `git check-ignore -v
base/a-shell` should report the repository's `/base/` rule.

Apple work is developed as source and narrow interfaces until an already
available Apple toolchain can validate it. A local Rust green check does not
prove Swift compilation, simulator behavior, device behavior, or App Store
readiness.

## Evidence discipline

Local tests prove local behavior only. They do not prove App Store review,
device performance, a-Shell compatibility, or package security. Those claims
need their own evidence and must remain explicitly unverified until tested.
