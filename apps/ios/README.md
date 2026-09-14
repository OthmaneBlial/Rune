# Rune iOS source

This directory contains the native SwiftUI frontend source and the narrow C
ABI declaration used to link `rune-ffi`. It is intentionally source-first:
this checkout does not download or install Xcode, an iOS SDK, simulators, or
third-party packages.

The runtime hand-off is documented in
[`../../docs/apple-validation.md`](../../docs/apple-validation.md). Until that
runbook is executed on a machine with an existing Apple toolchain, this
directory is source/API evidence only and is not an installable iOS build.

The SwiftUI view sends real command lines to the Rust session. It does not
invent terminal output. The Rust static library must be linked by the eventual
Xcode application target; a Swift package manifest alone is not an App Store
application project.

The default app session passes the real Documents, Library, and temporary
directories to Rust. Rune exposes them as `~`, `~/Library`, and `~/tmp` through
the confined VFS. A user-selected external folder deliberately uses only that
folder as its root and does not inherit the app's sibling directories.

The bridge also exposes a bounded newline-delimited Rust automation script entry
point for a future Shortcuts adapter. Rust rejects inputs above 256 KiB or 1,024
lines, then runs ordinary lines and the bounded multiline `for`, `if`, `while`,
`until`, `case`, and `NAME() { ... }` function constructs through the same
planner and registry while
preserving combined stdout, stderr, and the last status. Argument-free
`break`/`continue` are consumed by the innermost Rust-planned loop, and
`return [STATUS]` exits the current Rust-planned function with a bounded status.
`local NAME[=VALUE]` is scoped to the current Rust-planned function and is
restored when that function returns.
`shift [COUNT]` updates bounded positional parameters while preserving `$0`.
Native
Shortcuts registration remains unverified and is not included here.

`RuneShortcuts.swift` now declares source-only `AppIntent` actions for command
and script execution in both the default and named persisted Rust sessions.
They call the existing Rust FFI and return the real stdout/stderr/status
result; App Intent registration, entitlements, and Shortcuts runtime behavior
remain unverified without an Apple build.

The same file declares UTF-8 text Put/Get actions. Put writes through the Rust
VFS transfer API and Get reads through the binary-safe FFI, rejecting non-UTF-8
bytes before returning the text automation result. The Rust API itself accepts bounded
binary payloads up to 16 MiB; the source-only App Intent surface intentionally
does not claim arbitrary binary file parameters.

The terminal and workspace sources attach VoiceOver labels, hints, values, and
stable identifiers to their primary controls and session output. This provides
an explicit accessibility contract for the eventual native target; traversal,
Dynamic Type, contrast, and VoiceOver behavior remain unverified without an
Apple runtime.

Individual command lines crossing the bridge are also rejected above the
64-KiB Rust input limit before parsing or history recording.

The same Rust core also provides `source FILE [ARG ...]` and `. FILE [ARG ...]`
for bounded script files stored in the virtual filesystem. The Swift layer only
sends the command; file reads, nested execution limits, positional expansion,
environment changes, output, status, and history remain Rust-owned.

The source-only UI requests cooperative cancellation when a terminal view
disappears or its scene becomes inactive; this is a visibility-safety rule,
not a background execution claim. It can open a directory with the native
Files importer. The
Apple layer stores a bounded security-scoped bookmark, keeps the access scope
alive while the matching Rust session is active, and hands Rust only the
approved folder root. The settings sheet can reopen, rename, or remove saved
folder bookmarks without exposing bookmark bytes to Rust. This is source/API
evidence; picker behavior and entitlements remain unverified without an Apple
build/runtime.

The configuration query crosses the FFI as all twelve persisted Rust-owned keys,
including history redaction, cursor color, cursor shape, font, background, and foreground overrides; Swift
validates those values before applying them to the source-only view. This is
contract evidence, not proof of rendered appearance on Apple hardware.

Rendered transcript entries use a configurable 4,096-event default, bounded
to a maximum of 8,192 events, plus an 8 MiB byte cap in the SwiftUI model;
the oldest events are discarded when either bound is reached.
This is a display-memory policy and does not alter Rust command history or
files stored in the sandbox.

The Rust session additionally maintains a bounded cursor-grid snapshot with
split-chunk CSI/OSC parsing, cursor addressing, bounded scroll regions, and
character/line insertion and erasure. `RuneFFISession.terminalSnapshot` and
`RuneFFISession.terminalCursorPosition` and
`RuneFFISession.terminalCursorVisible` expose the
visible text, caret position, and bounded `CSI ?25l` / `CSI ?25h` visibility
state through the C ABI. The terminal view can switch to a source-only
Rust screen surface that draws this bounded text and caret; the styled,
line-oriented transcript remains the default, and Apple runtime rendering
still needs validation. The screen surface reports bounded row and column
dimensions through the C ABI and retains the most relevant rows when the
available iPhone/iPad space changes.

`RuneFFISession.diagnostics` exposes the Rust-owned bounded development log,
and `clearDiagnostics()` clears it in memory. The log is not persisted and is
limited to safe execution metadata rather than command text, file contents,
environment values, or private paths.

`RuneFFISession.cancel()` forwards a cooperative cancellation request to Rust;
the next command boundary returns status 130. It is a cancellation signal, not
an unsafe force-stop of a synchronous operation.

The bridge also exposes synchronous event-aware command/script calls. Rust
delivers borrowed output and status events through a C callback; Swift copies
them before the callback returns and feeds them into an `AsyncStream` consumed
by the main-actor terminal model while execution is still in progress. This is
boundary-level event delivery, not byte-level streaming from inside a WASM call;
Apple runtime rendering remains unverified.

The source-only bridge also installs an explicit bounded HTTP callback backed by
an ephemeral `URLSession`. Rust parses and validates `curl` requests, while
Swift owns URL loading and copies the response into a Rust-provided buffer;
there is no ambient socket access for the core or WASM. This is source/API
evidence only: URLSession, ATS configuration, TLS, redirects, and runtime
behavior remain unverified without an Apple build.
The bridge also installs an explicit external-open callback. Rust validates
approved URL schemes and confined file targets; `open` uses the normal file or
URL target, while `play` and `view` use explicit media/playback and preview
target kinds. RuneOpenBridge.swift hands those accepted targets to UIKit,
AVPlayer, or Quick Look on the main queue when available. The callback is an
asynchronous host acceptance boundary, so URL routing, document/media handling,
and completion remain unverified without an Apple runtime.
`RuneFFI.h` and `RuneCoreBridge.swift` also describe the explicit C/C++/TeX
toolchain callback. It provides Rune-owned output buffers and an aggregate
artifact arena, so a future provider can return copied, validated artifacts
without exposing arbitrary host paths or dangling pointers. No compiler, linker,
or TeX engine is included; toolchain execution remains unavailable until a
reviewed provider exists.
The same callback can serve Rust-owned package registry search and exact-version
package fetches; the registry index, manifest, artifact mapping, origin policy,
and SHA-256 checks remain in Rust. No registry URL or package bytes are stored
in Swift state.

The FFI session serializes mutable calls with a lock while allowing the atomic
cancellation signal to arrive from the UI thread. `RuneTerminalModel` runs
command execution in a detached task and reconnects the result to SwiftUI on
the main actor, so the source-only stop control can remain responsive. Swift
concurrency diagnostics and Apple runtime behavior remain unverified without
the Apple toolchain.

The Rust handle restores and persists the virtual working directory, typed
command history, and bounded bookmarks in `~/.rune/session.state` inside the
configured sandbox. It also stores a separate bounded text-only terminal
snapshot and zero-based caret position in `~/.rune/terminal.state`; named
sessions use the matching `sessions/{id}/` directory. Styles, scroll margins,
and incomplete control sequences are intentionally not persisted. User-defined
environment persistence is disabled by default; enabling the
`environment-persistence` setting enables its bounded,
explicitly opt-in state records while excluding core directory/runtime
variables. It may store exported values and is not a secret store.
FFI command and script calls flush that state before returning, while handle
destruction performs a final best-effort flush.
Aliases are not serialized. Environment variables are not serialized unless
the explicit opt-in configuration is enabled. History persistence is
intentionally visible in the local state boundary. Interactive `export`,
`setenv`, and assignment command lines are replaced by a redaction marker before
history is stored when `history-redaction` is enabled (the default); disabling it
is an explicit opt-out. The Rust core also persists bounded
`history-limit`, `history-redaction`, `environment-persistence`, `font`, `font-size`, `scrollback-limit`, `toolbar-visible`, `theme`,
`cursor-color`, `cursor-shape`, `background`, and `foreground` configuration in
`~/.rune/config.state`. The bridge exposes both key/value inspection and
validated set/reset calls without creating history entries; the source-only
Swift view provides a settings sheet for font, font size, scrollback, theme,
cursor color, cursor shape, background, foreground, history redaction,
environment persistence, reset, and Rust-persisted
toolbar visibility. The source-only UIKit command editor maps bar, block, and
underline to caret geometry when UIKit is available. Apple compilation and
runtime rendering remain unverified.

On restore, the Rust core reads a maximum of 64 KiB from the first existing
startup profile in this order: `~/.rune_profile`, `~/.profile`, then
`~/.bashrc`. It skips blank/full-line comment entries, runs supported Rune
script constructs and built-ins through the bounded Rust planner, and
surfaces the resulting output through the FFI. Profile commands, including
bounded one-command aliases and multiline functions, are not added to history;
the persisted working directory is restored after the profile.

`RuneWorkspaceView.swift` provides a source-only tab container. The first tab
uses the legacy default state file; additional tabs receive bounded opaque Rust
session IDs and persist their cwd/history/bookmarks below
`~/.rune/sessions/{id}/session.state`. This keeps tab state separate without
duplicating shell behavior in Swift. A bounded UserDefaults record now restores
tab titles, Rust session IDs, and the selected tab across launches without
persisting external paths or security-scoped bookmark bytes. The named
`WindowGroup` accepts a typed `RuneWindowRoute`; each additional iPad window
receives a distinct Rust session namespace, tab-metadata key, and selected-tab
key. This is source/API evidence only:
tab rendering, lifecycle behavior, scene restoration, and Apple runtime
integration remain unverified.

The Rust `exit`, `newWindow`, and `pickFolder` commands return one-shot
host-session actions over the same narrow FFI. The workspace consumes them
separately from terminal output: `exit` closes the selected tab or dismisses the
last window, `newWindow` opens a fresh typed window route with an independent
Rust session namespace, and `pickFolder` opens the existing confined folder
importer before reinstalling the session root. This is source/API evidence
only; SwiftUI lifecycle and Apple runtime behavior remain unverified.

The portable Rust registry also provides the short bookmark workflow aliases
`s`, `g`, `l`, `p`, `r`, and `d`. They operate on the same session-local map as
the full bookmark commands; the aliases are always available in Rune, while
the corresponding a-Shell shortcuts depend on its bookmark preferences.

`RuneFFISession.sessionSnapshot` decodes a versioned Rust-owned status payload
for the current tab. It contains only the session id, cwd, state counters, last
status, and terminal geometry/cursor; environment values and terminal text are
kept out of this metadata API. The terminal header uses those counters for
source-only session labeling.

## Current evidence

RuneOpenBridge.swift provides the source-only UIKit/AVPlayer/Quick Look adapter
for the Rust open/openurl/call/text/play/view capability; the portable CLI remains
launcher-disabled by default.

- `Package.swift` is a source/package boundary.
- `RuneFFI.h` documents the C ABI layout.
- `RuneCoreBridge.swift` owns and frees Rust session handles/strings.
- `RuneFFI.h` and `RuneCoreBridge.swift` expose the bounded toolchain callback
  boundary; they do not bundle a compiler or TeX engine.
- `RuneExternalFolderAccess.swift` owns bounded security-scoped bookmark
  storage and keeps approved folder access alive for a Rust session.
- `RuneShortcuts.swift` declares Rust-backed command and script actions for the
  default and named sessions, plus UTF-8 file App Intents.
- `RuneWorkspaceView.swift` declares the source-only independent-session tab
  container and Cmd-N new-window action.
- `RuneTerminalView.swift` declares source-only Command-key shortcuts for
  folder import, cancellation, history navigation, reverse history search, and
  command execution, plus
  a settings sheet backed by Rust configuration calls and an optional bounded
  input toolbar for Tab/completion, Escape, Ctrl-C, clear, and paste.
- `RuneClipboardBridge.swift` provides the source-only UIKit adapter for the
  Rust `pbcopy`/`pbpaste` capability. The callback is bounded to 1 MiB and the
  portable CLI remains clipboard-disabled by default.
- `RuneTerminalView.swift` includes the `@main` SwiftUI application entry point
  and launches the source-only workspace view.
- The UIKit command editor declares source-only hardware-key handlers for Tab,
  Escape, Ctrl-C, and Up/Down history navigation; they call the same model
  actions as the input toolbar and remain unverified without an Apple runtime.
- The bridge asks Rust for bounded command/path replacement candidates; it does
  not maintain a second command registry or filesystem listing in Swift.
- `RuneTerminalView.swift` renders stdout, stderr, and non-zero exit status
  separately, applies Rust-backed command/path suggestions, and provides the
  native focused command bar/history controls. Its history search panel calls
  the Rust-owned newest-first search endpoint and does not add search text to
  session history. It consumes Rune's clear-screen
  control sequence as a display action instead of showing escape bytes; the
  toolbar's display-clear action also resets and persists the Rust terminal
  grid without adding history. The
  source-only `RuneANSIText.swift` renderer also consumes common SGR foreground/
  background colors, 256-color/RGB colors, bold, underline, and inverse
  sequences; unsupported control sequences are omitted from display rather than
  shown as raw escape bytes.
- `swift package dump-package`, the isolated `RuneFFIHeaders` target build, and
  a real `swiftc -typecheck` pass with the already available macOS Swift
  toolchain. The check imports the C ABI module instead of maintaining
  duplicate Swift callback layouts; it is still not an iOS build or runtime
  proof.
- iOS compilation, simulator behavior, device behavior, and linking are
  currently **unverified** because no Apple build footprint is installed for
  this milestone.
