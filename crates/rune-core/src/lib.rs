//! Portable Rune command engine and session model.
//!
//! The engine is deliberately independent of the native Apple frontend. A
//! FFI consumers can expose its command/event model without moving shell
//! semantics into Swift.

mod clipboard;
mod commands;
mod config;
mod diagnostics;
mod network;
mod open;
mod persistence;
mod terminal;

pub use clipboard::{
    ClipboardError, ClipboardProvider, DisabledClipboardProvider, MAX_CLIPBOARD_BYTES,
};
pub use config::{
    TerminalBackground, TerminalConfig, TerminalCursorColor, TerminalCursorShape, TerminalFont,
    TerminalForeground, TerminalTheme,
};
pub use network::{
    DisabledNetworkProvider, NetworkError, NetworkMethod, NetworkProvider, NetworkRequest,
    NetworkResponse, MAX_NETWORK_BODY_BYTES, MAX_NETWORK_HEADERS, MAX_NETWORK_HEADER_BYTES,
    MAX_NETWORK_URL_BYTES,
};
pub use open::{
    DisabledOpenProvider, OpenError, OpenProvider, OpenRequest, OpenTargetKind,
    MAX_OPEN_TARGET_BYTES,
};
pub use rune_runtime::{
    DisabledToolchainProvider, ToolchainArtifact, ToolchainError, ToolchainKind, ToolchainOutput,
    ToolchainProvider, ToolchainRequest, MAX_TOOLCHAIN_ARGUMENTS, MAX_TOOLCHAIN_ARTIFACTS,
    MAX_TOOLCHAIN_ARTIFACT_BYTES, MAX_TOOLCHAIN_ARTIFACT_PATH_BYTES,
    MAX_TOOLCHAIN_ENVIRONMENT_BYTES, MAX_TOOLCHAIN_MEDIA_TYPE_BYTES, MAX_TOOLCHAIN_OUTPUT_BYTES,
    MAX_TOOLCHAIN_SOURCE_BYTES, MAX_TOOLCHAIN_STDIN_BYTES,
};

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use diagnostics::{DiagnosticLevel, DiagnosticLog};
use rune_fs::{FsError, VirtualFileSystem};
use rune_package::PackageManifest;
use rune_runtime::{
    JavaScriptRunner, LuaRunner, PythonRunner, Runtime, RuntimeKind, RuntimeRequest,
};
use rune_shell::{parse, CommandPlan, Connector, ExecutionPlan, Redirection, Word, WordPart};
use rune_wasm::WasmRunner;
use terminal::TerminalScreen;

const MAX_ALIAS_EXPANSIONS: usize = 32;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum UTF-8 payload in one output event delivered to native consumers.
pub const MAX_EVENT_CHUNK_BYTES: usize = 16 * 1024;
const MAX_COMPLETION_CANDIDATES: usize = 8;
const MAX_COMPLETION_INPUT_BYTES: usize = 64 * 1024;
const MAX_COMMAND_INPUT_BYTES: usize = 64 * 1024;
const MAX_HISTORY_SEARCH_BYTES: usize = 4 * 1024;
const MAX_INSTALLED_COMMANDS: usize = 4_096;
const MAX_SCRIPT_BYTES: usize = 256 * 1024;
const MAX_SCRIPT_LINES: usize = 1_024;
const MAX_SCRIPT_CONTROL_DEPTH: usize = 16;
const MAX_SOURCE_DEPTH: usize = 16;
const MAX_SOURCE_ARGUMENTS: usize = 64;
const MAX_COMMAND_SUBSTITUTION_DEPTH: usize = 16;
const MAX_FOR_VALUES: usize = 256;
const MAX_WHILE_ITERATIONS: usize = 1_024;
const MAX_CASE_PATTERNS: usize = 64;
const MAX_FUNCTIONS: usize = 256;
const MAX_FUNCTION_NAME_BYTES: usize = 64;
const MAX_FUNCTION_ARGUMENTS: usize = 64;
const MAX_FUNCTION_DEPTH: usize = 16;
const MAX_LOCAL_VARIABLES: usize = 64;
const MAX_LOCAL_NAME_BYTES: usize = 64;
pub(crate) const CANCELLED_STATUS: i32 = 130;
const MAX_BOOKMARKS: usize = 256;
const MAX_BOOKMARK_NAME_CHARS: usize = 64;
const MAX_BOOKMARK_PATH_BYTES: usize = 64 * 1024;
const MAX_BOOKMARK_BYTES: usize = 256 * 1024;
const MAX_DIRECTORY_USAGE_ENTRIES: usize = 1_024;
const MAX_DIRECTORY_USAGE_BYTES: usize = 256 * 1024;
/// Maximum payload accepted by the explicit native file-transfer boundary.
pub const MAX_FILE_TRANSFER_BYTES: usize = 16 * 1024 * 1024;
const OUTPUT_TRUNCATION_MARKER: &str = "\n[rune: output truncated at 1048576 bytes]\n";
pub(crate) const PACKAGE_INSTALL_ROOT: &str = "~/.rune/packages";

fn supports_path_completion(command: &str) -> bool {
    matches!(
        command,
        "ar" | "awk"
            | "base64"
            | "bc"
            | "basename"
            | "cat"
            | "c++"
            | "cd"
            | "cc"
            | "cksum"
            | "clang"
            | "clang++"
            | "cp"
            | "compress"
            | "cut"
            | "curl"
            | "diff"
            | "du"
            | "egrep"
            | "expr"
            | "file"
            | "find"
            | "fgrep"
            | "grep"
            | "gunzip"
            | "gzip"
            | "head"
            | "jsc"
            | "ls"
            | "ln"
            | "lua"
            | "md5"
            | "mktemp"
            | "mkdir"
            | "mv"
            | "open"
            | "python"
            | "python3"
            | "readlink"
            | "realpath"
            | "rm"
            | "rmdir"
            | "sed"
            | "sha256"
            | "source"
            | "stat"
            | "tail"
            | "tar"
            | "tee"
            | "tex"
            | "touch"
            | "tree"
            | "unzip"
            | "uncompress"
            | "unlink"
            | "wasm"
            | "xargs"
            | "xxd"
            | "zip"
            | "z"
            | "."
    )
}

fn directories_only_for_completion(command: &str) -> bool {
    matches!(command, "cd" | "mkdir" | "rmdir" | "z")
}

fn completion_command_segment(input: &str) -> Option<&str> {
    if input.matches('|').count() > 1 || input.contains("||") {
        return None;
    }
    Some(
        input
            .rsplit_once('|')
            .map_or(input, |(_, segment)| segment)
            .trim_start(),
    )
}

fn history_command_name(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find(|token| !is_history_assignment(token))
        .filter(|token| is_completion_name(token))
}

fn is_history_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut characters = name.chars();
    matches!(
        characters.next(),
        Some(character) if character == '_' || character.is_ascii_alphabetic()
    ) && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn is_completion_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '_' | '-' | '.' | '+' | '/' | '[' | ']' | '~')
        })
}

struct InstalledCommand {
    name: String,
    package: String,
    manifest_path: String,
    module_path: String,
    entry: String,
    filesystem_access: bool,
}

struct InstalledInvocation<'a> {
    program: &'a str,
    arguments: &'a [String],
    stdin: &'a str,
    record_history: bool,
    source_depth: usize,
    sink: &'a mut dyn EventSink,
    command: &'a InstalledCommand,
}

struct ToolchainProviders {
    c: Box<dyn ToolchainProvider>,
    cpp: Box<dyn ToolchainProvider>,
    tex: Box<dyn ToolchainProvider>,
}

impl Default for ToolchainProviders {
    fn default() -> Self {
        Self {
            c: Box::new(DisabledToolchainProvider::new(ToolchainKind::C)),
            cpp: Box::new(DisabledToolchainProvider::new(ToolchainKind::Cpp)),
            tex: Box::new(DisabledToolchainProvider::new(ToolchainKind::Tex)),
        }
    }
}

impl ToolchainProviders {
    fn provider(&self, kind: ToolchainKind) -> &dyn ToolchainProvider {
        match kind {
            ToolchainKind::C => self.c.as_ref(),
            ToolchainKind::Cpp => self.cpp.as_ref(),
            ToolchainKind::Tex => self.tex.as_ref(),
        }
    }

    fn set(&mut self, provider: Box<dyn ToolchainProvider>) {
        match provider.kind() {
            ToolchainKind::C => self.c = provider,
            ToolchainKind::Cpp => self.cpp = provider,
            ToolchainKind::Tex => self.tex = provider,
        }
    }
}

fn is_wasm_entry(entry: &str) -> bool {
    std::path::Path::new(entry)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"))
}

fn is_rune_script_entry(entry: &str) -> bool {
    std::path::Path::new(entry)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rune"))
}

fn is_lua_entry(entry: &str) -> bool {
    std::path::Path::new(entry)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("lua"))
}

fn is_javascript_entry(entry: &str) -> bool {
    std::path::Path::new(entry)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("js"))
}

fn is_python_entry(entry: &str) -> bool {
    std::path::Path::new(entry)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OutputTarget {
    Stdout,
    Stderr,
    File {
        path: String,
        append: bool,
        descriptor: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedRedirections {
    stdin: String,
    stdout: OutputTarget,
    stderr: OutputTarget,
}

/// The result of one command or complete command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

/// A host-facing session action requested by a Rust command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    Exit,
    NewWindow,
    PickFolder,
}

/// A bounded, non-secret snapshot of one Rust-owned terminal session.
///
/// The snapshot is intentionally metadata rather than an environment dump:
/// native hosts can label tabs and inspect state without receiving exported
/// values or duplicating session bookkeeping in Swift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub id: String,
    pub working_directory: String,
    pub history_count: usize,
    pub bookmark_count: usize,
    pub environment_count: usize,
    pub terminal_columns: usize,
    pub terminal_rows: usize,
    pub terminal_cursor_row: usize,
    pub terminal_cursor_column: usize,
    pub last_status: i32,
}

impl SessionSnapshot {
    /// Serializes the stable versioned snapshot consumed by native hosts.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "schema_version": 1,
            "id": self.id,
            "working_directory": self.working_directory,
            "history_count": self.history_count,
            "bookmark_count": self.bookmark_count,
            "environment_count": self.environment_count,
            "last_status": self.last_status,
            "terminal_state": {
                "columns": self.terminal_columns,
                "rows": self.terminal_rows,
                "cursor": {
                    "row": self.terminal_cursor_row,
                    "column": self.terminal_cursor_column,
                },
            },
        })
        .to_string()
    }
}

impl CommandOutput {
    #[must_use]
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            status: 0,
        }
    }

    #[must_use]
    pub fn failure(status: i32, stderr: impl Into<String>) -> Self {
        Self {
            stdout: String::new(),
            stderr: stderr.into(),
            status,
        }
    }
}

/// A bounded event emitted at a Rust execution boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandEvent {
    /// A bounded output chunk visible after one pipeline has completed.
    /// Redirections have already been applied, so redirected bytes are not
    /// emitted here.
    Output { stdout: String, stderr: String },
    /// Status and virtual directory after one command line or script line.
    Status {
        status: i32,
        current_directory: String,
    },
}

/// Receives execution events without owning or mutating a [`Session`].
pub trait EventSink {
    fn emit(&mut self, event: CommandEvent);
}

struct NoopEventSink;

impl EventSink for NoopEventSink {
    fn emit(&mut self, _event: CommandEvent) {}
}

struct TrackingEventSink<'a> {
    sink: &'a mut dyn EventSink,
    output_emitted: bool,
}

struct TerminalEventSink<'a> {
    sink: &'a mut dyn EventSink,
    screen: &'a mut TerminalScreen,
}

struct ScriptExecutionContext<'a> {
    record_history: bool,
    source_depth: usize,
    external_stdin: &'a str,
    control_depth: usize,
    sink: &'a mut dyn EventSink,
}

#[derive(Debug, Clone)]
struct FunctionDefinition {
    body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopControl {
    Break,
    Continue,
}

impl<'a> TrackingEventSink<'a> {
    fn new(sink: &'a mut dyn EventSink) -> Self {
        Self {
            sink,
            output_emitted: false,
        }
    }
}

impl EventSink for TrackingEventSink<'_> {
    fn emit(&mut self, event: CommandEvent) {
        if matches!(event, CommandEvent::Output { .. }) {
            self.output_emitted = true;
        }
        self.sink.emit(event);
    }
}

impl EventSink for TerminalEventSink<'_> {
    fn emit(&mut self, event: CommandEvent) {
        if let CommandEvent::Output { stdout, stderr } = &event {
            self.screen.feed(stdout);
            self.screen.feed(stderr);
        }
        self.sink.emit(event);
    }
}

/// A command handler in the registry.
pub type CommandHandler = for<'a> fn(&mut CommandContext<'a>) -> CommandOutput;

/// Registry metadata for one built-in command.
#[derive(Debug, Clone, Copy)]
pub struct CommandDefinition {
    pub name: &'static str,
    pub summary: &'static str,
    pub handler: CommandHandler,
}

/// Registry of commands available to a session.
#[derive(Debug, Clone, Copy)]
pub struct CommandRegistry {
    definitions: &'static [CommandDefinition],
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self {
            definitions: commands::DEFINITIONS,
        }
    }
}

impl CommandRegistry {
    #[must_use]
    pub fn definitions(&self) -> &[CommandDefinition] {
        self.definitions
    }

    fn find(&self, name: &str) -> Option<CommandHandler> {
        self.definitions
            .iter()
            .find(|definition| definition.name == name)
            .map(|definition| definition.handler)
    }
}

/// Context passed to one registered command.
pub struct CommandContext<'a> {
    pub(crate) args: &'a [String],
    pub(crate) stdin: &'a str,
    pub(crate) fs: &'a mut dyn VirtualFileSystem,
    pub(crate) env: &'a mut BTreeMap<String, String>,
    pub(crate) aliases: &'a mut BTreeMap<String, String>,
    pub(crate) bookmarks: &'a mut BTreeMap<String, String>,
    pub(crate) directory_usage: &'a mut BTreeMap<String, u32>,
    pub(crate) config: &'a mut TerminalConfig,
    pub(crate) history: &'a mut Vec<String>,
    pub(crate) command_definitions: &'a [CommandDefinition],
    pub(crate) runtime: &'a dyn Runtime,
    pub(crate) python_runtime: &'a dyn Runtime,
    pub(crate) lua_runtime: &'a dyn Runtime,
    pub(crate) javascript_runtime: &'a dyn Runtime,
    pub(crate) toolchains: &'a ToolchainProviders,
    pub(crate) network: &'a dyn NetworkProvider,
    pub(crate) clipboard: &'a dyn ClipboardProvider,
    pub(crate) opener: &'a dyn OpenProvider,
    pub(crate) cancellation: &'a AtomicBool,
}

impl CommandContext<'_> {
    pub(crate) fn take_cancellation(&self) -> Option<CommandOutput> {
        self.cancellation
            .swap(false, Ordering::AcqRel)
            .then(|| CommandOutput::failure(CANCELLED_STATUS, "rune: command cancelled\n"))
    }
}

/// One independent terminal session.
pub struct Session {
    filesystem: Box<dyn VirtualFileSystem>,
    environment: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
    bookmarks: BTreeMap<String, String>,
    directory_usage: BTreeMap<String, u32>,
    config: TerminalConfig,
    history: Vec<String>,
    history_limit: usize,
    registry: CommandRegistry,
    last_status: i32,
    startup_output: CommandOutput,
    wasm_runner: WasmRunner,
    python_runner: PythonRunner,
    lua_runner: LuaRunner,
    javascript_runner: JavaScriptRunner,
    toolchains: ToolchainProviders,
    network_provider: Box<dyn NetworkProvider>,
    clipboard_provider: Box<dyn ClipboardProvider>,
    open_provider: Box<dyn OpenProvider>,
    cancellation_requested: Arc<AtomicBool>,
    command_substitution_depth: usize,
    loop_depth: usize,
    loop_control: Option<LoopControl>,
    functions: BTreeMap<String, FunctionDefinition>,
    function_depth: usize,
    function_return: Option<i32>,
    function_local_bindings: Vec<BTreeMap<String, Option<String>>>,
    state_session_id: Option<String>,
    script_parameters: Vec<String>,
    terminal_screen: TerminalScreen,
    diagnostics: DiagnosticLog,
    pending_action: Option<SessionAction>,
}

impl Session {
    /// Creates a session with the supplied filesystem policy.
    pub fn new(filesystem: impl VirtualFileSystem + 'static) -> Self {
        let mut environment = BTreeMap::new();
        environment.insert("HOME".to_string(), "~".to_string());
        environment.insert("PATH".to_string(), "~/.rune/bin".to_string());
        environment.insert(
            "RUNE_VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        );
        environment.insert("TERM".to_string(), "rune".to_string());
        let mut session = Self {
            filesystem: Box::new(filesystem),
            environment,
            aliases: BTreeMap::new(),
            bookmarks: BTreeMap::new(),
            directory_usage: BTreeMap::new(),
            config: TerminalConfig::default(),
            history: Vec::new(),
            history_limit: 1_000,
            registry: CommandRegistry::default(),
            last_status: 0,
            startup_output: CommandOutput::success(""),
            wasm_runner: WasmRunner::default(),
            python_runner: PythonRunner,
            lua_runner: LuaRunner,
            javascript_runner: JavaScriptRunner,
            toolchains: ToolchainProviders::default(),
            network_provider: Box::new(DisabledNetworkProvider),
            clipboard_provider: Box::new(DisabledClipboardProvider),
            open_provider: Box::new(DisabledOpenProvider),
            cancellation_requested: Arc::new(AtomicBool::new(false)),
            command_substitution_depth: 0,
            loop_depth: 0,
            loop_control: None,
            functions: BTreeMap::new(),
            function_depth: 0,
            function_return: None,
            function_local_bindings: Vec::new(),
            state_session_id: None,
            script_parameters: Vec::new(),
            terminal_screen: TerminalScreen::default(),
            diagnostics: DiagnosticLog::default(),
            pending_action: None,
        };
        session.update_pwd();
        session
    }

    /// Restores current directory and command history from the sandbox state.
    ///
    /// Invalid or missing state is ignored and produces a fresh session. The
    /// Environment values are restored only when the explicit
    /// `environment-persistence` configuration is enabled.
    pub fn restore(filesystem: impl VirtualFileSystem + 'static) -> Self {
        Self::restore_with_namespace(filesystem, None)
    }

    /// Restores an independent session using a bounded persistence namespace.
    ///
    /// The namespace separates cwd, history, and bookmarks from Rune's legacy
    /// default session state while keeping the virtual filesystem root shared.
    ///
    /// # Errors
    ///
    /// Returns an invalid-path error when `session_id` contains unsupported
    /// characters or exceeds the persistence bound.
    pub fn restore_with_id(
        filesystem: impl VirtualFileSystem + 'static,
        session_id: &str,
    ) -> Result<Self, FsError> {
        if !persistence::is_valid_session_id(session_id) {
            return Err(FsError::InvalidPath(format!(
                "invalid Rune session id: {session_id}"
            )));
        }
        Ok(Self::restore_with_namespace(
            filesystem,
            Some(session_id.to_string()),
        ))
    }

    fn restore_with_namespace(
        filesystem: impl VirtualFileSystem + 'static,
        state_session_id: Option<String>,
    ) -> Self {
        let mut session = Self::new(filesystem);
        session.state_session_id = state_session_id;
        session.config = TerminalConfig::load(session.filesystem.as_ref());
        session.history_limit = session.config.history_limit();
        let state = persistence::load(
            session.filesystem.as_ref(),
            session.state_session_id.as_deref(),
        );
        if let Some(terminal) = state.terminal.as_ref() {
            session.terminal_screen.restore_persisted_state(terminal);
        }
        session.directory_usage = state.directory_usage;
        session.load_startup_profile();
        session.history = state.history;
        session.apply_history_limit();
        session.bookmarks = state.bookmarks;
        if session.config.environment_persistence() {
            session.environment.extend(state.environment);
        }
        if let Some(directory) = state.current_directory {
            let _ = session.filesystem.change_dir(&directory);
        }
        session.update_pwd();
        session.last_status = 0;
        session
    }

    /// Takes output produced while loading the selected startup profile during restore.
    /// Profile commands are intentionally not added to history.
    pub fn take_startup_output(&mut self) -> CommandOutput {
        std::mem::replace(&mut self.startup_output, CommandOutput::success(""))
    }

    /// Requests cooperative cancellation at the next execution boundary.
    ///
    /// The request is consumed by the next command, pipeline, or script
    /// boundary observed by the session. A synchronous operation already in
    /// progress is allowed to finish; this method does not deliver host
    /// signals or forcefully stop a runtime.
    pub fn cancel(&self) {
        self.cancellation_requested.store(true, Ordering::Release);
    }

    /// Installs the host-owned network capability used by network commands.
    ///
    /// The default session has no network provider. A native adapter may
    /// install a bounded URLSession-backed provider without moving sockets or
    /// platform APIs into the Rust command engine.
    pub fn set_network_provider(&mut self, provider: Box<dyn NetworkProvider>) {
        self.network_provider = provider;
    }

    /// Installs the host-owned text clipboard capability used by `pbcopy` and
    /// `pbpaste`. The default session has no clipboard provider.
    pub fn set_clipboard_provider(&mut self, provider: Box<dyn ClipboardProvider>) {
        self.clipboard_provider = provider;
    }

    /// Installs the host-owned external application capability used by open
    /// openurl, call, and text. The default session has no launcher provider.
    pub fn set_open_provider(&mut self, provider: Box<dyn OpenProvider>) {
        self.open_provider = provider;
    }

    /// Installs one explicit C, C++, or TeX toolchain capability.
    ///
    /// The default session keeps all toolchain providers unavailable. A host
    /// or package integration may install a reviewed provider that returns
    /// artifacts for materialization through Rune's confined VFS; providers
    /// never receive an ambient host path or shell.
    pub fn set_toolchain_provider(&mut self, provider: Box<dyn ToolchainProvider>) {
        self.toolchains.set(provider);
    }

    /// Returns the bridge handle used to request cancellation safely while
    /// the Rust session is executing on another thread.
    #[must_use]
    pub fn cancellation_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancellation_requested)
    }

    /// Persists the current virtual directory, history, bookmarks, and a
    /// bounded text-only terminal screen snapshot.
    /// User-defined environment values are included only when the explicit
    /// `environment-persistence` setting is enabled.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the state directory cannot be
    /// created or the state file cannot be written.
    pub fn persist(&mut self) -> Result<(), FsError> {
        let directory = self.filesystem.current_dir_display();
        let persisted_environment = self
            .config
            .environment_persistence()
            .then_some(&self.environment);
        let terminal = self.terminal_screen.persisted_state();
        let state = persistence::SessionStateView {
            current_directory: &directory,
            history: &self.history,
            bookmarks: &self.bookmarks,
            directory_usage: &self.directory_usage,
            environment: persisted_environment,
            terminal: &terminal,
        };
        persistence::save(
            self.filesystem.as_mut(),
            &state,
            self.state_session_id.as_deref(),
        )?;
        self.config.save(self.filesystem.as_mut())
    }

    /// Clears the Rust-owned terminal screen without recording a shell
    /// command, then persists the empty screen for the next launch.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the cleared session cannot be
    /// persisted.
    pub fn clear_terminal_screen(&mut self) -> Result<(), FsError> {
        self.terminal_screen.reset();
        self.record_diagnostic(DiagnosticLevel::Info, "terminal", "display cleared");
        let result = self.persist();
        if result.is_err() {
            self.record_diagnostic(
                DiagnosticLevel::Error,
                "persistence",
                "display clear could not be persisted",
            );
        }
        result
    }

    /// Returns the bounded, non-persistent diagnostic log for development and
    /// support tooling. It contains safe metadata only; command text, file
    /// contents, environment values, and private paths are excluded by policy.
    #[must_use]
    pub fn diagnostics(&self) -> String {
        self.diagnostics.snapshot()
    }

    /// Clears the in-memory diagnostic log without changing session state.
    pub fn clear_diagnostics(&mut self) {
        self.diagnostics.clear();
    }

    /// Consumes one host-facing action requested by a Rust command.
    #[must_use]
    pub fn take_action(&mut self) -> Option<SessionAction> {
        self.pending_action.take()
    }

    /// Returns the current virtual directory, useful to native frontends.
    #[must_use]
    pub fn current_directory(&self) -> String {
        self.filesystem.current_dir_display()
    }

    /// Returns the read-only environment snapshot.
    #[must_use]
    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    /// Returns the session-local command aliases.
    #[must_use]
    pub fn aliases(&self) -> &BTreeMap<String, String> {
        &self.aliases
    }

    /// Returns the session-local directory bookmarks.
    #[must_use]
    pub fn bookmarks(&self) -> &BTreeMap<String, String> {
        &self.bookmarks
    }

    /// Returns bounded session metadata without exposing environment values.
    #[must_use]
    pub fn snapshot(&self) -> SessionSnapshot {
        let (terminal_columns, terminal_rows) = self.terminal_screen.dimensions();
        let (terminal_cursor_row, terminal_cursor_column) = self.terminal_screen.cursor_position();
        SessionSnapshot {
            id: self
                .state_session_id
                .as_deref()
                .unwrap_or("default")
                .to_string(),
            working_directory: self.current_directory(),
            history_count: self.history.len(),
            bookmark_count: self.bookmarks.len(),
            environment_count: self.environment.len(),
            terminal_columns,
            terminal_rows,
            terminal_cursor_row,
            terminal_cursor_column,
            last_status: self.last_status,
        }
    }

    /// Returns the current portable Rust-owned terminal configuration.
    #[must_use]
    pub fn configuration(&self) -> &TerminalConfig {
        &self.config
    }

    /// Updates one Rust-owned terminal setting without recording a shell
    /// command in history. Native settings surfaces use this method so UI
    /// changes share the same validation and persistence policy as `config`.
    pub fn set_configuration(&mut self, key: &str, value: &str) -> CommandOutput {
        let output = match config::update(self.filesystem.as_mut(), &mut self.config, key, value) {
            Ok(()) => {
                if key == "history-limit" {
                    self.apply_history_limit();
                }
                CommandOutput::success("")
            }
            Err(message) => CommandOutput::failure(2, format!("config: {message}\n")),
        };
        self.last_status = output.status;
        output
    }

    /// Restores Rust-owned terminal settings to their defaults without adding
    /// a synthetic command to history.
    pub fn reset_configuration(&mut self) -> CommandOutput {
        let output = config::reset(self.filesystem.as_mut(), &mut self.config).map_or_else(
            |error| fs_failure("config", &error),
            |()| CommandOutput::success(""),
        );
        if output.status == 0 {
            self.apply_history_limit();
        }
        self.last_status = output.status;
        output
    }

    /// Returns the command history in execution order.
    #[must_use]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Searches the session history from newest to oldest without recording a
    /// synthetic command. The query is bounded independently from command
    /// execution so native reverse-search controls cannot create an unbounded
    /// allocation or mutate shell state.
    #[must_use]
    pub fn search_history(&self, query: &str) -> Option<Vec<String>> {
        if query.len() > MAX_HISTORY_SEARCH_BYTES {
            return None;
        }
        let query = query.to_lowercase();
        Some(
            self.history
                .iter()
                .rev()
                .filter(|entry| entry.to_lowercase().contains(&query))
                .cloned()
                .collect(),
        )
    }

    /// Reads one bounded file through the session's confined filesystem.
    ///
    /// This is the data boundary used by native automation. Shell `cat` keeps
    /// its own output-channel limit; direct file transfer has a separate byte
    /// limit so an API caller cannot allocate an unbounded result.
    ///
    /// # Errors
    ///
    /// Returns the confined filesystem error, including a transfer-limit
    /// error when the file is larger than [`MAX_FILE_TRANSFER_BYTES`].
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let bytes = self.filesystem.read(path)?;
        if bytes.len() > MAX_FILE_TRANSFER_BYTES {
            return Err(FsError::Io {
                operation: "read".to_string(),
                path: path.to_string(),
                message: format!("file exceeds the {MAX_FILE_TRANSFER_BYTES}-byte transfer limit"),
            });
        }
        Ok(bytes)
    }

    /// Writes one bounded file through the session's confined filesystem.
    ///
    /// The operation replaces the file and never creates parent directories.
    /// Callers must explicitly create directories through the shell or another
    /// bounded filesystem operation.
    ///
    /// # Errors
    ///
    /// Returns the confined filesystem error or a transfer-limit error when
    /// `content` is larger than [`MAX_FILE_TRANSFER_BYTES`].
    pub fn write_file(&mut self, path: &str, content: &[u8]) -> Result<(), FsError> {
        if content.len() > MAX_FILE_TRANSFER_BYTES {
            return Err(FsError::Io {
                operation: "write".to_string(),
                path: path.to_string(),
                message: format!("file exceeds the {MAX_FILE_TRANSFER_BYTES}-byte transfer limit"),
            });
        }
        self.filesystem.write(path, content, false)
    }

    /// Returns the last command status.
    #[must_use]
    pub fn last_status(&self) -> i32 {
        self.last_status
    }

    /// Returns the bounded Rust-owned terminal screen after the latest
    /// command or script execution. The snapshot contains visible text only;
    /// native renderers may continue to apply event-local styling separately.
    #[must_use]
    pub fn terminal_snapshot(&self) -> String {
        self.terminal_screen.snapshot()
    }

    /// Returns the zero-based cursor position in the bounded Rust-owned
    /// terminal screen.
    #[must_use]
    pub fn terminal_cursor_position(&self) -> (usize, usize) {
        self.terminal_screen.cursor_position()
    }

    /// Returns whether the bounded terminal screen requests a visible caret.
    #[must_use]
    pub fn terminal_cursor_visible(&self) -> bool {
        self.terminal_screen.cursor_visible()
    }

    /// Returns the optional terminal-requested cursor shape override.
    #[must_use]
    pub fn terminal_cursor_shape(&self) -> u8 {
        self.terminal_screen.cursor_shape()
    }

    /// Returns the optional terminal-requested cursor blink override.
    #[must_use]
    pub fn terminal_cursor_blink(&self) -> u8 {
        self.terminal_screen.cursor_blink()
    }

    /// Resizes the Rust-owned terminal grid for a native viewport. The
    /// bounded screen keeps the most relevant rows and clamps dimensions;
    /// layout changes are intentionally not persisted as session state.
    pub fn resize_terminal(&mut self, columns: usize, rows: usize) {
        self.terminal_screen.resize(columns, rows);
        self.record_diagnostic(
            DiagnosticLevel::Info,
            "terminal",
            format!("viewport resized columns={columns} rows={rows}"),
        );
    }

    /// Returns the current registry metadata for UI completion/help.
    #[must_use]
    pub fn commands(&self) -> &[CommandDefinition] {
        self.registry.definitions()
    }

    /// Returns bounded command or path completion candidates owned by Rust.
    ///
    /// Candidates are replacement tokens rather than complete command lines.
    /// The native frontend can therefore preserve the already-entered command
    /// and arguments while applying the selected token. Quoted fragments,
    /// escaped text, and compound shell operators are left untouched until the
    /// completion grammar can return a structured replacement range.
    #[must_use]
    pub fn completion_candidates(&self, input: &str) -> Vec<String> {
        if input.len() > MAX_COMPLETION_INPUT_BYTES {
            return Vec::new();
        }
        if input.is_empty()
            || input
                .chars()
                .any(|character| ";&\\\"'#".contains(character))
        {
            return Vec::new();
        }

        let Some(command_segment) = completion_command_segment(input) else {
            return Vec::new();
        };
        if !command_segment.chars().any(char::is_whitespace) {
            return self.command_completion_candidates(command_segment);
        }

        self.path_completion_candidates(input)
    }

    /// Applies one Rust-owned completion candidate to the final whitespace
    /// separated token in the input line.
    ///
    /// The candidate must be one of the bounded candidates returned for the
    /// same input. This keeps replacement-range and suffix rules in the core
    /// instead of duplicating shell text handling in native frontends.
    #[must_use]
    pub fn apply_completion(&self, input: &str, candidate: &str) -> Option<String> {
        if input.len() > MAX_COMPLETION_INPUT_BYTES
            || candidate.is_empty()
            || candidate.len() > 128
            || input
                .chars()
                .any(|character| ";&\\\"'#".contains(character))
            || !is_completion_name(candidate)
            || !self
                .completion_candidates(input)
                .iter()
                .any(|value| value == candidate)
        {
            return None;
        }
        let token_start = input
            .char_indices()
            .rev()
            .find(|(_, character)| character.is_whitespace())
            .map_or(0, |(offset, character)| offset + character.len_utf8());
        let prefix = &input[..token_start];
        let suffix = if candidate.ends_with('/') { "" } else { " " };
        Some(format!("{prefix}{candidate}{suffix}"))
    }

    fn command_completion_candidates(&self, prefix: &str) -> Vec<String> {
        let mut candidates = self
            .registry
            .definitions()
            .iter()
            .map(|definition| definition.name)
            .filter(|name| *name != prefix && name.starts_with(prefix))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        candidates.extend(
            self.aliases
                .keys()
                .filter(|name| name.as_str() != prefix && name.starts_with(prefix))
                .cloned(),
        );
        candidates.extend(
            self.history
                .iter()
                .filter_map(|entry| history_command_name(entry))
                .filter(|name| *name != prefix && name.starts_with(prefix))
                .map(str::to_owned),
        );
        if let Ok(installed_commands) = installed_commands_in_filesystem(self.filesystem.as_ref()) {
            candidates.extend(
                installed_commands
                    .into_iter()
                    .map(|command| command.name)
                    .filter(|name| *name != prefix && name.starts_with(prefix)),
            );
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates.truncate(MAX_COMPLETION_CANDIDATES);
        candidates
    }

    fn completion_entries(
        &self,
        command: &str,
        directory: &str,
        path_prefix: &str,
        name_prefix: &str,
    ) -> Vec<String> {
        let Ok(entries) = self.filesystem.list(Some(directory)) else {
            return Vec::new();
        };
        let directories_only = directories_only_for_completion(command);
        let mut candidates = entries
            .into_iter()
            .filter(|entry| {
                (!directories_only || entry.is_directory)
                    && entry.name.starts_with(name_prefix)
                    && entry.name != name_prefix
                    && entry.name.chars().all(|character| {
                        !character.is_whitespace() && !"|;&<>\\\"'#$*?".contains(character)
                    })
            })
            .map(|entry| {
                let suffix = if entry.is_directory { "/" } else { "" };
                format!("{path_prefix}{}{suffix}", entry.name)
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.dedup();
        candidates.truncate(MAX_COMPLETION_CANDIDATES);
        candidates
    }

    fn bookmark_completion_candidates(&self, command: &str, token: &str) -> Option<Vec<String>> {
        let rest = token.strip_prefix('~')?;
        if rest.starts_with('/') {
            return None;
        }
        let Some(slash) = rest.find('/') else {
            let mut candidates = self
                .bookmarks
                .keys()
                .filter(|name| name.starts_with(rest))
                .map(|name| format!("~{name}/"))
                .collect::<Vec<_>>();
            candidates.sort_unstable();
            candidates.truncate(MAX_COMPLETION_CANDIDATES);
            return Some(candidates);
        };
        if slash == 0 {
            return None;
        }
        let name = &rest[..slash];
        let bookmark_path = self.bookmarks.get(name)?;
        let final_slash = token.rfind('/')?;
        let directory_suffix = &token[(name.len() + 1)..final_slash];
        let directory = format!("{bookmark_path}{directory_suffix}");
        let path_prefix = &token[..=final_slash];
        let name_prefix = &token[final_slash + 1..];
        Some(self.completion_entries(command, &directory, path_prefix, name_prefix))
    }

    fn path_completion_candidates(&self, input: &str) -> Vec<String> {
        let active_start = input.rfind('|').map_or(0, |index| index + 1);
        let active = &input[active_start..];
        let leading = active.len() - active.trim_start().len();
        let command_start = active_start + leading;
        let command_end = input[command_start..]
            .find(char::is_whitespace)
            .map_or(input.len(), |offset| command_start + offset);
        let command = &input[command_start..command_end];
        let token_start = input
            .char_indices()
            .rev()
            .find(|(_, character)| character.is_whitespace())
            .map_or(0, |(offset, character)| offset + character.len_utf8());
        let redirection_target = input[..token_start].trim_end().ends_with(['<', '>']);
        if !supports_path_completion(command) && !redirection_target {
            return Vec::new();
        }

        let token = &input[token_start..];
        if token.starts_with('-') {
            return Vec::new();
        }

        if let Some(candidates) = self.bookmark_completion_candidates(command, token) {
            return candidates;
        }

        let (directory, path_prefix, name_prefix) = match token.rfind('/') {
            Some(slash) => {
                let directory = if slash == 0 { "/" } else { &token[..slash] };
                (directory, &token[..=slash], &token[slash + 1..])
            }
            None => (".", "", token),
        };
        if path_prefix.is_empty() {
            let Ok(entries) = self.filesystem.list(None) else {
                return Vec::new();
            };
            let directories_only = directories_only_for_completion(command);
            let mut candidates = entries
                .into_iter()
                .filter(|entry| {
                    (!directories_only || entry.is_directory)
                        && entry.name.starts_with(name_prefix)
                        && entry.name != name_prefix
                        && entry.name.chars().all(|character| {
                            !character.is_whitespace() && !"|;&<>\\\"'#$*?".contains(character)
                        })
                })
                .map(|entry| {
                    let suffix = if entry.is_directory { "/" } else { "" };
                    format!("{path_prefix}{}{suffix}", entry.name)
                })
                .collect::<Vec<_>>();
            candidates.sort_unstable();
            candidates.dedup();
            candidates.truncate(MAX_COMPLETION_CANDIDATES);
            candidates
        } else {
            self.completion_entries(command, directory, path_prefix, name_prefix)
        }
    }

    /// Executes one parsed command line and returns separate output channels.
    pub fn execute_line(&mut self, input: &str) -> CommandOutput {
        let mut sink = NoopEventSink;
        self.execute_with_terminal(&mut sink, |session, sink| {
            session.execute_line_internal(input, true, 0, "", sink)
        })
    }

    /// Executes one command line and emits bounded output/status events.
    pub fn execute_line_with_events(
        &mut self,
        input: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        self.execute_with_terminal(sink, |session, sink| {
            session.execute_line_internal(input, true, 0, "", sink)
        })
    }

    /// Executes a bounded newline-delimited automation script.
    ///
    /// Empty lines are ignored. Ordinary lines and bounded `for`, `if`,
    /// `while`, and `until` constructs use the same parser and command
    /// registry as interactive input; loop bodies may be multiline or use the
    /// bounded `for ...; do command; done` form. Execution continues after a
    /// failed command so automation can observe the complete output. The
    /// returned status is the status of the last executed construct.
    pub fn execute_script(&mut self, script: &str) -> CommandOutput {
        let mut sink = NoopEventSink;
        self.execute_with_terminal(&mut sink, |session, sink| {
            session.execute_script_internal(script, true, 0, "", sink)
        })
    }

    /// Executes a bounded script and emits events for each executed line.
    pub fn execute_script_with_events(
        &mut self,
        script: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        self.execute_with_terminal(sink, |session, sink| {
            session.execute_script_internal(script, true, 0, "", sink)
        })
    }

    fn execute_with_terminal<F>(&mut self, sink: &mut dyn EventSink, execute: F) -> CommandOutput
    where
        F: FnOnce(&mut Self, &mut dyn EventSink) -> CommandOutput,
    {
        let mut screen = std::mem::take(&mut self.terminal_screen);
        let output = {
            let mut terminal_sink = TerminalEventSink {
                sink,
                screen: &mut screen,
            };
            execute(self, &mut terminal_sink)
        };
        self.terminal_screen = screen;
        self.record_diagnostic(
            if output.status == 0 {
                DiagnosticLevel::Info
            } else {
                DiagnosticLevel::Warn
            },
            "execution",
            format!(
                "completed status={} stdout_bytes={} stderr_bytes={}",
                output.status,
                output.stdout.len(),
                output.stderr.len()
            ),
        );
        output
    }

    fn record_diagnostic(
        &mut self,
        level: DiagnosticLevel,
        component: &'static str,
        message: impl AsRef<str>,
    ) {
        self.diagnostics.record(level, component, message);
    }

    fn execute_script_internal(
        &mut self,
        script: &str,
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        let mut tracking = TrackingEventSink::new(sink);
        let mut context = ScriptExecutionContext {
            record_history,
            source_depth,
            external_stdin,
            control_depth: 0,
            sink: &mut tracking,
        };
        let output = self.execute_script_body(script, &mut context);
        if !tracking.output_emitted && (!output.stdout.is_empty() || !output.stderr.is_empty()) {
            emit_output_chunks(&mut tracking, &output.stdout, &output.stderr);
        }
        tracking.emit(CommandEvent::Status {
            status: output.status,
            current_directory: self.filesystem.current_dir_display(),
        });
        output
    }

    fn script_line_limit_failure(&mut self) -> CommandOutput {
        let output = CommandOutput::failure(
            2,
            format!("rune: script expands beyond the {MAX_SCRIPT_LINES}-line limit\n"),
        );
        self.last_status = output.status;
        output
    }

    fn execute_script_body(
        &mut self,
        script: &str,
        context: &mut ScriptExecutionContext<'_>,
    ) -> CommandOutput {
        if context.control_depth > MAX_SCRIPT_CONTROL_DEPTH {
            let output = CommandOutput::failure(
                2,
                format!(
                    "rune: script control-flow nesting exceeds the {MAX_SCRIPT_CONTROL_DEPTH}-level limit\n"
                ),
            );
            self.last_status = output.status;
            return output;
        }
        if script.len() > MAX_SCRIPT_BYTES {
            let output = CommandOutput::failure(
                2,
                format!("rune: script exceeds the {MAX_SCRIPT_BYTES}-byte input limit\n"),
            );
            self.last_status = output.status;
            return output;
        }
        if script.lines().count() > MAX_SCRIPT_LINES {
            let output = CommandOutput::failure(
                2,
                format!("rune: script exceeds the {MAX_SCRIPT_LINES}-line input limit\n"),
            );
            self.last_status = output.status;
            return output;
        }
        let normalized_lines = normalized_script_lines(script);
        if normalized_lines.len() > MAX_SCRIPT_LINES {
            return self.script_line_limit_failure();
        }
        let lines = normalized_lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut output = CommandOutput::success("");
        let mut line_index = 0;
        while line_index < lines.len() {
            let line = lines[line_index];
            if line.trim().is_empty() {
                line_index += 1;
                continue;
            }
            if let Some(cancellation) = self.take_cancellation() {
                output.stderr.push_str(&cancellation.stderr);
                output.status = cancellation.status;
                break;
            }
            match self.execute_script_control(&lines, line_index, context) {
                Ok(Some((next_line, construct_output))) => {
                    output.stdout.push_str(&construct_output.stdout);
                    output.stderr.push_str(&construct_output.stderr);
                    output.status = construct_output.status;
                    limit_output(&mut output);
                    line_index = next_line;
                    if output.status == CANCELLED_STATUS {
                        break;
                    }
                    if self.pending_action.is_some()
                        || self.loop_control.is_some()
                        || self.function_return.is_some()
                    {
                        break;
                    }
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    let error_output =
                        CommandOutput::failure(2, format!("rune: script: {error}\n"));
                    output.stderr.push_str(&error_output.stderr);
                    output.status = error_output.status;
                    limit_output(&mut output);
                    self.last_status = output.status;
                    break;
                }
            }
            let line_output = self.execute_line_internal(
                line,
                context.record_history,
                context.source_depth,
                context.external_stdin,
                context.sink,
            );
            output.stdout.push_str(&line_output.stdout);
            output.stderr.push_str(&line_output.stderr);
            output.status = line_output.status;
            limit_output(&mut output);
            line_index += 1;
            if self.pending_action.is_some()
                || self.loop_control.is_some()
                || self.function_return.is_some()
            {
                break;
            }
        }
        output
    }

    fn execute_script_control(
        &mut self,
        lines: &[&str],
        line_index: usize,
        context: &mut ScriptExecutionContext<'_>,
    ) -> Result<Option<(usize, CommandOutput)>, String> {
        let line = lines[line_index];
        if let Some((name, body)) = parse_inline_function_definition(line)? {
            if context.record_history {
                self.record_history_line(line.trim());
            }
            self.register_function(name, body)?;
            self.last_status = 0;
            return Ok(Some((line_index + 1, CommandOutput::success(""))));
        }
        if is_function_header_line(line) {
            let name = parse_function_header(line)?
                .ok_or_else(|| "invalid function header".to_string())?;
            let (body_start, body_end) = function_body_range(lines, line_index)?;
            if context.record_history {
                self.record_history_line(line.trim());
            }
            self.register_function(name, lines[body_start..body_end].join("\n"))?;
            self.last_status = 0;
            return Ok(Some((body_end + 1, CommandOutput::success(""))));
        }
        if is_if_header_line(line) {
            let header =
                parse_if_header(line, "if")?.ok_or_else(|| "invalid `if` header".to_string())?;
            let block = parse_if_block(lines, line_index, &header)?;
            if context.record_history {
                self.record_history_line(line.trim());
            }
            let output = self.execute_if_block(&block, context);
            return Ok(Some((block.end + 1, output)));
        }
        if is_for_header_line(line) {
            let header =
                parse_for_header(line)?.ok_or_else(|| "invalid `for` header".to_string())?;
            let (body_start, body_end) = loop_body_range(lines, line_index, header.inline_do)?;
            if context.record_history {
                self.record_history_line(line.trim());
            }
            let body = lines[body_start..body_end].join("\n");
            let output = self.execute_for_loop(&header, &body, context);
            return Ok(Some((body_end + 1, output)));
        }
        if is_loop_header_line(line) {
            let header = parse_while_header(line)?
                .ok_or_else(|| "invalid `while`/`until` header".to_string())?;
            let (body_start, body_end) = loop_body_range(lines, line_index, header.inline_do)?;
            if context.record_history {
                self.record_history_line(line.trim());
            }
            let body = lines[body_start..body_end].join("\n");
            let output = self.execute_while_loop(&header, &body, context);
            return Ok(Some((body_end + 1, output)));
        }
        if is_case_header_line(line) {
            let header =
                parse_case_header(line)?.ok_or_else(|| "invalid `case` header".to_string())?;
            let block = parse_case_block(lines, line_index, &header)?;
            if context.record_history {
                self.record_history_line(line.trim());
            }
            let output = self.execute_case_block(&block, context);
            return Ok(Some((block.end + 1, output)));
        }
        Ok(None)
    }

    fn execute_for_loop(
        &mut self,
        header: &ForHeader,
        body: &str,
        context: &mut ScriptExecutionContext<'_>,
    ) -> CommandOutput {
        let mut values = Vec::new();
        let mut output = CommandOutput::success("");
        for word in &header.values {
            let expanded = match self.expand_word(word, context.source_depth) {
                Ok(expanded) => expanded,
                Err(error) => {
                    output.stdout.push_str(&error.stdout);
                    output.stderr.push_str(&error.stderr);
                    output.status = error.status;
                    limit_output(&mut output);
                    self.last_status = error.status;
                    return output;
                }
            };
            output.stderr.push_str(&expanded.stderr);
            if expanded.has_wildcard {
                let matches = match self.filesystem.glob(&expanded.value) {
                    Ok(matches) => matches,
                    Err(error) => {
                        let error_output = fs_failure("for", &error);
                        output.stdout.push_str(&error_output.stdout);
                        output.stderr.push_str(&error_output.stderr);
                        output.status = error_output.status;
                        limit_output(&mut output);
                        self.last_status = error_output.status;
                        return output;
                    }
                };
                values.extend(matches);
            } else {
                values.push(expanded.value);
            }
            if values.len() > MAX_FOR_VALUES {
                let error_output = CommandOutput::failure(
                    2,
                    format!("for: value list exceeds the {MAX_FOR_VALUES}-item limit\n"),
                );
                output.stderr.push_str(&error_output.stderr);
                output.status = error_output.status;
                limit_output(&mut output);
                self.last_status = error_output.status;
                return output;
            }
        }

        self.loop_depth += 1;
        for (index, value) in values.into_iter().enumerate() {
            if let Some(cancellation) = self.take_cancellation() {
                output.stderr.push_str(&cancellation.stderr);
                output.status = cancellation.status;
                break;
            }
            self.environment.insert(header.variable.clone(), value);
            let iteration_stdin = if index == 0 {
                context.external_stdin
            } else {
                ""
            };
            let mut iteration_context = ScriptExecutionContext {
                record_history: context.record_history,
                source_depth: context.source_depth,
                external_stdin: iteration_stdin,
                control_depth: context.control_depth + 1,
                sink: &mut *context.sink,
            };
            let iteration = self.execute_script_body(body, &mut iteration_context);
            output.stdout.push_str(&iteration.stdout);
            output.stderr.push_str(&iteration.stderr);
            output.status = iteration.status;
            limit_output(&mut output);
            if output.status == CANCELLED_STATUS {
                break;
            }
            if self.function_return.is_some() {
                break;
            }
            match self.loop_control.take() {
                Some(LoopControl::Break) => break,
                Some(LoopControl::Continue) | None => {}
            }
        }
        self.loop_depth -= 1;
        self.last_status = output.status;
        output
    }

    fn register_function(&mut self, name: String, body: String) -> Result<(), String> {
        if !self.functions.contains_key(&name) && self.functions.len() >= MAX_FUNCTIONS {
            return Err(format!(
                "function definition limit exceeds {MAX_FUNCTIONS} functions"
            ));
        }
        self.functions.insert(name, FunctionDefinition { body });
        Ok(())
    }

    fn execute_function(
        &mut self,
        name: &str,
        arguments: &[String],
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        _sink: &mut dyn EventSink,
    ) -> CommandOutput {
        if arguments.len() > MAX_FUNCTION_ARGUMENTS {
            return usage(
                name,
                &format!("usage: {name} [ARG ...] (up to {MAX_FUNCTION_ARGUMENTS} arguments)"),
            );
        }
        if self.function_depth >= MAX_FUNCTION_DEPTH {
            return CommandOutput::failure(
                2,
                format!(
                    "{name}: function recursion exceeds the {MAX_FUNCTION_DEPTH}-level limit\n"
                ),
            );
        }
        let Some(definition) = self.functions.get(name).cloned() else {
            return CommandOutput::failure(127, format!("{name}: command not found\n"));
        };
        let previous_return = self.function_return.take();
        let previous_parameters = self.script_parameters.clone();
        let mut parameters = Vec::with_capacity(arguments.len() + 1);
        parameters.push(
            previous_parameters
                .first()
                .cloned()
                .unwrap_or_else(|| name.to_string()),
        );
        parameters.extend(arguments.iter().cloned());
        self.script_parameters = parameters;
        self.function_depth += 1;
        self.function_local_bindings.push(BTreeMap::new());
        // A function is one command from the caller's event stream. Its
        // internal lines must not be emitted once here and once again as the
        // function's aggregate pipeline result.
        let mut nested_sink = NoopEventSink;
        let output = self.execute_script_internal(
            &definition.body,
            record_history,
            source_depth,
            external_stdin,
            &mut nested_sink,
        );
        let returned_status = self.function_return.take();
        self.function_return = previous_return;
        let local_bindings = self.function_local_bindings.pop().unwrap_or_default();
        for (variable, previous_value) in local_bindings {
            if let Some(previous_value) = previous_value {
                self.environment.insert(variable, previous_value);
            } else {
                self.environment.remove(&variable);
            }
        }
        self.function_depth -= 1;
        self.script_parameters = previous_parameters;
        let mut output = output;
        if let Some(status) = returned_status {
            output.status = status;
        }
        self.last_status = output.status;
        output
    }

    fn execute_local(&mut self, arguments: &[String]) -> CommandOutput {
        if self.function_depth == 0 || self.function_local_bindings.is_empty() {
            return CommandOutput::failure(2, "local: only valid inside a function\n");
        }
        if arguments.is_empty() || arguments.len() > MAX_LOCAL_VARIABLES {
            return usage(
                "local",
                &format!("usage: local NAME[=VALUE] ... (up to {MAX_LOCAL_VARIABLES} variables)"),
            );
        }
        let mut assignments = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let (name, value) = argument
                .split_once('=')
                .map_or((argument.as_str(), ""), |(name, value)| (name, value));
            if !is_valid_script_variable(name) {
                return CommandOutput::failure(
                    2,
                    format!("local: invalid variable name: {name}\n"),
                );
            }
            if name.len() > MAX_LOCAL_NAME_BYTES {
                return CommandOutput::failure(
                    2,
                    format!("local: variable name exceeds the {MAX_LOCAL_NAME_BYTES}-byte limit\n"),
                );
            }
            assignments.push((name.to_string(), value.to_string()));
        }
        for (name, value) in assignments {
            let previous_value = self.environment.get(&name).cloned();
            if let Some(frame) = self.function_local_bindings.last_mut() {
                frame.entry(name.clone()).or_insert(previous_value);
            } else {
                return CommandOutput::failure(2, "local: function frame is unavailable\n");
            }
            self.environment.insert(name, value);
        }
        CommandOutput::success("")
    }

    fn execute_while_loop(
        &mut self,
        header: &WhileHeader,
        body: &str,
        context: &mut ScriptExecutionContext<'_>,
    ) -> CommandOutput {
        let mut output = CommandOutput::success("");
        let mut iterations = 0;
        self.loop_depth += 1;
        loop {
            if let Some(cancellation) = self.take_cancellation() {
                output.stderr.push_str(&cancellation.stderr);
                output.status = cancellation.status;
                break;
            }
            let condition = self.execute_line_internal(
                &header.condition,
                false,
                context.source_depth,
                if iterations == 0 {
                    context.external_stdin
                } else {
                    ""
                },
                &mut *context.sink,
            );
            output.stdout.push_str(&condition.stdout);
            output.stderr.push_str(&condition.stderr);
            if self.function_return.is_some() {
                break;
            }
            match self.loop_control.take() {
                Some(LoopControl::Break) => break,
                Some(LoopControl::Continue) => continue,
                None => {}
            }
            let condition_succeeded = condition.status == 0;
            let should_execute = if header.until {
                !condition_succeeded
            } else {
                condition_succeeded
            };
            if !should_execute {
                if iterations == 0 {
                    output.status = 0;
                }
                break;
            }
            if iterations >= MAX_WHILE_ITERATIONS {
                let error = CommandOutput::failure(
                    2,
                    format!(
                        "{}: loop exceeds the {MAX_WHILE_ITERATIONS}-iteration limit\n",
                        if header.until { "until" } else { "while" }
                    ),
                );
                output.stderr.push_str(&error.stderr);
                output.status = error.status;
                break;
            }
            let iteration_stdin = if iterations == 0 {
                context.external_stdin
            } else {
                ""
            };
            let mut body_context = ScriptExecutionContext {
                record_history: context.record_history,
                source_depth: context.source_depth,
                external_stdin: iteration_stdin,
                control_depth: context.control_depth + 1,
                sink: &mut *context.sink,
            };
            let iteration = self.execute_script_body(body, &mut body_context);
            output.stdout.push_str(&iteration.stdout);
            output.stderr.push_str(&iteration.stderr);
            output.status = iteration.status;
            limit_output(&mut output);
            iterations += 1;
            if output.status == CANCELLED_STATUS {
                break;
            }
            if self.function_return.is_some() {
                break;
            }
            match self.loop_control.take() {
                Some(LoopControl::Break) => break,
                Some(LoopControl::Continue) | None => {}
            }
        }
        self.loop_depth -= 1;
        limit_output(&mut output);
        self.last_status = output.status;
        output
    }

    fn execute_case_block(
        &mut self,
        block: &CaseBlock,
        context: &mut ScriptExecutionContext<'_>,
    ) -> CommandOutput {
        let expanded = match self.expand_word(&block.word, context.source_depth) {
            Ok(expanded) => expanded,
            Err(error) => {
                self.last_status = error.status;
                return error;
            }
        };
        let mut output = CommandOutput::success("");
        output.stderr.push_str(&expanded.stderr);
        if let Some(clause) = block.clauses.iter().find(|clause| {
            clause
                .patterns
                .iter()
                .any(|pattern| case_pattern_matches(pattern, &expanded.value))
        }) {
            let mut body_context = ScriptExecutionContext {
                record_history: context.record_history,
                source_depth: context.source_depth,
                external_stdin: context.external_stdin,
                control_depth: context.control_depth + 1,
                sink: &mut *context.sink,
            };
            let body_output = self.execute_script_body(&clause.body, &mut body_context);
            output.stdout.push_str(&body_output.stdout);
            output.stderr.push_str(&body_output.stderr);
            output.status = body_output.status;
        }
        limit_output(&mut output);
        self.last_status = output.status;
        output
    }

    fn execute_if_block(
        &mut self,
        block: &IfBlock,
        context: &mut ScriptExecutionContext<'_>,
    ) -> CommandOutput {
        let mut output = CommandOutput::success("");
        let mut selected = false;
        for clause in &block.clauses {
            if let Some(condition) = &clause.condition {
                let condition_output = self.execute_line_internal(
                    condition,
                    false,
                    context.source_depth,
                    context.external_stdin,
                    &mut *context.sink,
                );
                output.stdout.push_str(&condition_output.stdout);
                output.stderr.push_str(&condition_output.stderr);
                if condition_output.status != 0 {
                    continue;
                }
            }
            selected = true;
            let mut body_context = ScriptExecutionContext {
                record_history: context.record_history,
                source_depth: context.source_depth,
                external_stdin: context.external_stdin,
                control_depth: context.control_depth + 1,
                sink: &mut *context.sink,
            };
            let body_output = self.execute_script_body(&clause.body, &mut body_context);
            output.stdout.push_str(&body_output.stdout);
            output.stderr.push_str(&body_output.stderr);
            output.status = body_output.status;
            break;
        }
        if !selected {
            output.status = 0;
        }
        limit_output(&mut output);
        self.last_status = output.status;
        output
    }

    fn execute_line_internal(
        &mut self,
        input: &str,
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        let mut tracking = TrackingEventSink::new(sink);
        let output = self.execute_line_body(
            input,
            record_history,
            source_depth,
            external_stdin,
            &mut tracking,
        );
        if !tracking.output_emitted && (!output.stdout.is_empty() || !output.stderr.is_empty()) {
            emit_output_chunks(&mut tracking, &output.stdout, &output.stderr);
        }
        tracking.emit(CommandEvent::Status {
            status: output.status,
            current_directory: self.filesystem.current_dir_display(),
        });
        output
    }

    fn execute_line_body(
        &mut self,
        input: &str,
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        if input.len() > MAX_COMMAND_INPUT_BYTES {
            let output = CommandOutput::failure(
                2,
                format!(
                    "rune: command line exceeds the {MAX_COMMAND_INPUT_BYTES}-byte input limit\n"
                ),
            );
            self.last_status = output.status;
            return output;
        }
        let line = input.trim_matches(['\r', '\n', ' ']);
        if line.is_empty() {
            return CommandOutput::success("");
        }
        if let Some(output) = self.take_cancellation() {
            self.last_status = output.status;
            return output;
        }
        match parse_inline_function_definition(line) {
            Ok(Some((name, body))) => {
                if record_history {
                    self.record_history_line(line);
                }
                if let Err(error) = self.register_function(name, body) {
                    let output = CommandOutput::failure(2, format!("rune: parse: {error}\n"));
                    self.last_status = output.status;
                    return output;
                }
                self.last_status = 0;
                return CommandOutput::success("");
            }
            Ok(None) => {}
            Err(error) => {
                let output = CommandOutput::failure(2, format!("rune: parse: {error}\n"));
                self.last_status = output.status;
                return output;
            }
        }
        if record_history {
            self.record_history_line(line);
        }

        let plan = match parse(line) {
            Ok(plan) => plan,
            Err(error) => {
                let output = CommandOutput::failure(2, format!("rune: parse: {error}\n"));
                self.last_status = output.status;
                return output;
            }
        };
        if plan.is_empty() {
            return CommandOutput::success("");
        }
        let mut output =
            self.execute_plan(&plan, record_history, source_depth, external_stdin, sink);
        limit_output(&mut output);
        output
    }

    fn record_history_line(&mut self, line: &str) {
        let entry = history_entry(line, self.config.history_redaction());
        if self.history.last() != Some(&entry) {
            self.history.push(entry);
            persistence::apply_history_limit(&mut self.history, self.history_limit);
        }
    }

    fn execute_builtin(
        &mut self,
        arguments: &[String],
        stdin: &str,
        handler: CommandHandler,
    ) -> CommandOutput {
        let mut context = CommandContext {
            args: arguments,
            stdin,
            fs: self.filesystem.as_mut(),
            env: &mut self.environment,
            aliases: &mut self.aliases,
            bookmarks: &mut self.bookmarks,
            directory_usage: &mut self.directory_usage,
            config: &mut self.config,
            history: &mut self.history,
            command_definitions: self.registry.definitions(),
            runtime: &self.wasm_runner,
            python_runtime: &self.python_runner,
            lua_runtime: &self.lua_runner,
            javascript_runtime: &self.javascript_runner,
            toolchains: &self.toolchains,
            network: self.network_provider.as_ref(),
            clipboard: self.clipboard_provider.as_ref(),
            opener: self.open_provider.as_ref(),
            cancellation: &self.cancellation_requested,
        };
        handler(&mut context)
    }

    fn load_startup_profile(&mut self) {
        let lines = match persistence::load_profile(self.filesystem.as_ref()) {
            Ok(lines) => lines,
            Err(error) => {
                self.startup_output.status = 1;
                let _ = writeln!(self.startup_output.stderr, "rune: profile: {error}");
                return;
            }
        };
        if lines.is_empty() {
            return;
        }
        let script = lines.join("\n");
        let mut sink = NoopEventSink;
        let output = self.execute_with_terminal(&mut sink, |session, sink| {
            session.execute_script_internal(&script, false, 0, "", sink)
        });
        self.startup_output.stdout.push_str(&output.stdout);
        self.startup_output.stderr.push_str(&output.stderr);
        if output.status != 0 {
            self.startup_output.status = output.status;
            let _ = writeln!(
                self.startup_output.stderr,
                "rune: profile script exited with status {}",
                output.status
            );
        }
        // A startup profile initializes shell state; host actions are only
        // meaningful for an explicit interactive command submission.
        self.pending_action = None;
    }

    fn execute_plan(
        &mut self,
        plan: &ExecutionPlan,
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        let mut output = CommandOutput::success("");
        for (index, pipeline) in plan.pipelines.iter().enumerate() {
            if let Some(cancellation) = self.take_cancellation() {
                output.stderr.push_str(&cancellation.stderr);
                output.status = cancellation.status;
                self.last_status = output.status;
                break;
            }
            if index > 0 {
                let should_skip = match plan.connectors[index - 1] {
                    Connector::And => output.status != 0,
                    Connector::Or => output.status == 0,
                    Connector::Sequence => false,
                };
                if should_skip {
                    continue;
                }
            }
            let pipeline_output =
                self.execute_pipeline(pipeline, record_history, source_depth, external_stdin, sink);
            let mut event_output = pipeline_output.clone();
            limit_output(&mut event_output);
            emit_output_chunks(sink, &event_output.stdout, &event_output.stderr);
            output.stdout.push_str(&pipeline_output.stdout);
            output.stderr.push_str(&pipeline_output.stderr);
            output.status = pipeline_output.status;
            if self.pending_action.is_some() {
                break;
            }
        }
        self.last_status = output.status;
        output
    }

    fn execute_pipeline(
        &mut self,
        pipeline: &rune_shell::PipelinePlan,
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        let mut stdin = external_stdin.to_string();
        let mut stderr = String::new();
        let mut status = 0;
        for command in &pipeline.commands {
            if let Some(output) = self.take_cancellation() {
                return output;
            }
            let mut result =
                self.execute_command(command, &stdin, record_history, source_depth, sink);
            limit_output(&mut result);
            stdin = result.stdout;
            stderr.push_str(&result.stderr);
            status = result.status;
            if self.pending_action.is_some() {
                break;
            }
        }
        CommandOutput {
            stdout: stdin,
            stderr,
            status,
        }
    }

    fn execute_command(
        &mut self,
        command: &CommandPlan,
        external_stdin: &str,
        record_history: bool,
        source_depth: usize,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        self.execute_command_with_aliases(
            command,
            external_stdin,
            0,
            record_history,
            source_depth,
            sink,
        )
    }

    fn execute_command_with_aliases(
        &mut self,
        command: &CommandPlan,
        external_stdin: &str,
        depth: usize,
        record_history: bool,
        source_depth: usize,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        let previous_directory = self.filesystem.current_dir_display();
        let expanded_command = match self.expand_alias(command, depth) {
            Ok(expanded) => expanded,
            Err(error) => return CommandOutput::failure(2, format!("rune: alias: {error}\n")),
        };
        if let Some(expanded_command) = expanded_command {
            return self.execute_command_with_aliases(
                &expanded_command,
                external_stdin,
                depth + 1,
                record_history,
                source_depth,
                sink,
            );
        }

        let mut expansion_stderr = String::new();
        for assignment in &command.assignments {
            let expanded = match self.expand_word(&assignment.value, source_depth) {
                Ok(expanded) => expanded,
                Err(output) => return output,
            };
            expansion_stderr.push_str(&expanded.stderr);
            let value = expanded.value;
            self.environment.insert(assignment.name.clone(), value);
        }
        let (program, arguments, command_stderr) =
            match self.expand_command_words(command, source_depth) {
                Ok(expanded) => expanded,
                Err(output) => return output,
            };
        expansion_stderr.push_str(&command_stderr);
        let (redirections, redirection_stderr) =
            match self.apply_redirections(command, &program, external_stdin, source_depth) {
                Ok(redirections) => redirections,
                Err(output) => return output,
            };
        expansion_stderr.push_str(&redirection_stderr);

        let mut output = if command.program.parts().is_empty() && !command.assignments.is_empty() {
            CommandOutput::success("")
        } else {
            self.execute_expanded_command(
                &program,
                &arguments,
                &redirections.stdin,
                record_history,
                source_depth,
                sink,
            )
        };
        if !expansion_stderr.is_empty() {
            expansion_stderr.push_str(&output.stderr);
            output.stderr = expansion_stderr;
        }
        self.apply_history_limit();
        self.update_directory_environment(previous_directory);
        self.apply_output_redirections(&program, &mut output, redirections);
        self.last_status = output.status;
        output
    }

    fn execute_expanded_command(
        &mut self,
        program: &str,
        arguments: &[String],
        external_stdin: &str,
        record_history: bool,
        source_depth: usize,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        match program {
            "break" | "continue" => self.execute_loop_control(program, arguments),
            "exit" => self.execute_session_action(program, arguments, SessionAction::Exit),
            "newWindow" | "new-window" => {
                self.execute_session_action(program, arguments, SessionAction::NewWindow)
            }
            "pickFolder" => {
                self.execute_session_action(program, arguments, SessionAction::PickFolder)
            }
            "return" => self.execute_function_return(arguments),
            "local" => self.execute_local(arguments),
            "shift" => self.execute_shift(arguments),
            "set" => self.execute_set(arguments),
            "source" | "." => self.execute_source(
                program,
                arguments,
                record_history,
                source_depth,
                external_stdin,
                sink,
            ),
            "sh" | "dash" => self.execute_shell_command(
                program,
                arguments,
                record_history,
                source_depth,
                external_stdin,
                sink,
            ),
            "xargs" => self.execute_xargs(arguments, external_stdin, source_depth, sink),
            "command" if !matches!(arguments.first().map(String::as_str), Some("-v" | "-V")) => {
                self.execute_command_builtin(
                    arguments,
                    external_stdin,
                    record_history,
                    source_depth,
                    sink,
                )
            }
            _ if self.functions.contains_key(program) => self.execute_function(
                program,
                arguments,
                record_history,
                source_depth,
                external_stdin,
                sink,
            ),
            _ => self.execute_registered_or_installed(
                program,
                arguments,
                external_stdin,
                record_history,
                source_depth,
                sink,
            ),
        }
    }

    fn execute_loop_control(&mut self, command: &str, arguments: &[String]) -> CommandOutput {
        if !arguments.is_empty() {
            return usage(command, &format!("usage: {command}"));
        }
        if self.loop_depth == 0 {
            return CommandOutput::failure(2, format!("{command}: only valid inside a loop\n"));
        }
        self.loop_control = Some(if command == "break" {
            LoopControl::Break
        } else {
            LoopControl::Continue
        });
        CommandOutput::success("")
    }

    fn execute_session_action(
        &mut self,
        command: &str,
        arguments: &[String],
        action: SessionAction,
    ) -> CommandOutput {
        if !arguments.is_empty() {
            return usage(command, &format!("usage: {command}"));
        }
        self.pending_action = Some(action);
        CommandOutput::success("")
    }

    fn execute_function_return(&mut self, arguments: &[String]) -> CommandOutput {
        if self.function_depth == 0 {
            return CommandOutput::failure(2, "return: only valid inside a function\n");
        }
        if arguments.len() > 1 {
            return usage("return", "usage: return [STATUS]");
        }
        let status = match arguments.first() {
            None => self.last_status,
            Some(argument) => match argument.parse::<i32>() {
                Ok(status) if (0..=255).contains(&status) => status,
                _ => {
                    return CommandOutput::failure(
                        2,
                        "return: status must be an integer from 0 through 255\n",
                    )
                }
            },
        };
        self.function_return = Some(status);
        CommandOutput::success("")
    }

    fn execute_shift(&mut self, arguments: &[String]) -> CommandOutput {
        if self.function_depth == 0 && self.script_parameters.len() <= 1 {
            return CommandOutput::failure(2, "shift: only valid inside a script or function\n");
        }
        if arguments.len() > 1 {
            return usage("shift", "usage: shift [COUNT]");
        }
        let count = match arguments.first() {
            None => 1,
            Some(argument) => match argument.parse::<usize>() {
                Ok(count) => count,
                Err(_) => {
                    return CommandOutput::failure(
                        2,
                        "shift: count must be a non-negative integer\n",
                    )
                }
            },
        };
        let positional_count = self.script_parameters.len().saturating_sub(1);
        if count > positional_count {
            return CommandOutput::failure(
                2,
                format!("shift: count {count} exceeds {positional_count} positional arguments\n"),
            );
        }
        if count > 0 {
            self.script_parameters.drain(1..=count);
        }
        CommandOutput::success("")
    }

    fn execute_set(&mut self, arguments: &[String]) -> CommandOutput {
        if arguments.first().map(String::as_str) != Some("--")
            || arguments.len().saturating_sub(1) > MAX_SOURCE_ARGUMENTS
        {
            return usage(
                "set",
                &format!(
                    "usage: set -- [ARG ...] (up to {MAX_SOURCE_ARGUMENTS} positional arguments)"
                ),
            );
        }
        let name = self.script_parameters.first().cloned().unwrap_or_default();
        let mut parameters = Vec::with_capacity(arguments.len());
        parameters.push(name);
        parameters.extend(arguments.iter().skip(1).cloned());
        self.script_parameters = parameters;
        CommandOutput::success("")
    }

    fn execute_registered_or_installed(
        &mut self,
        program: &str,
        arguments: &[String],
        external_stdin: &str,
        record_history: bool,
        source_depth: usize,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        if let Some(handler) = self.registry.find(program) {
            return self.execute_builtin(arguments, external_stdin, handler);
        }
        let installed_command = match self.find_installed_command(program) {
            Ok(command) => command,
            Err(error) => return fs_failure(program, &error),
        };
        let Some(installed_command) = installed_command else {
            return CommandOutput::failure(127, format!("{program}: command not found\n"));
        };
        let mut invocation = InstalledInvocation {
            program,
            arguments,
            stdin: external_stdin,
            record_history,
            source_depth,
            sink,
            command: &installed_command,
        };
        self.execute_installed(&mut invocation)
    }

    fn execute_command_builtin(
        &mut self,
        arguments: &[String],
        external_stdin: &str,
        record_history: bool,
        source_depth: usize,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        let arguments = if arguments.first().map(String::as_str) == Some("--") {
            &arguments[1..]
        } else {
            arguments
        };
        let Some(program) = arguments.first() else {
            return usage("command", "usage: command COMMAND [ARG ...]");
        };
        let command_arguments = &arguments[1..];
        let source_command = matches!(program.as_str(), "source" | ".");
        let shell_command = matches!(program.as_str(), "sh" | "dash");
        let xargs_command = program == "xargs";
        if source_command {
            return self.execute_source(
                program,
                command_arguments,
                record_history,
                source_depth,
                external_stdin,
                sink,
            );
        }
        if shell_command {
            return self.execute_shell_command(
                program,
                command_arguments,
                record_history,
                source_depth,
                external_stdin,
                sink,
            );
        }
        if xargs_command {
            return self.execute_xargs(command_arguments, external_stdin, source_depth, sink);
        }
        if matches!(program.as_str(), "break" | "continue") {
            return self.execute_loop_control(program, command_arguments);
        }
        if program == "return" {
            return self.execute_function_return(command_arguments);
        }
        if program == "local" {
            return self.execute_local(command_arguments);
        }
        if program == "shift" {
            return self.execute_shift(command_arguments);
        }
        if program == "set" {
            return self.execute_set(command_arguments);
        }
        if self.functions.contains_key(program) {
            return self.execute_function(
                program,
                command_arguments,
                record_history,
                source_depth,
                external_stdin,
                sink,
            );
        }
        if let Some(handler) = self.registry.find(program) {
            return self.execute_builtin(command_arguments, external_stdin, handler);
        }
        let installed_command = match self.find_installed_command(program) {
            Ok(command) => command,
            Err(error) => return fs_failure(program, &error),
        };
        let Some(installed_command) = installed_command else {
            return CommandOutput::failure(127, format!("{program}: command not found\n"));
        };
        let mut invocation = InstalledInvocation {
            program,
            arguments: command_arguments,
            stdin: external_stdin,
            record_history,
            source_depth,
            sink,
            command: &installed_command,
        };
        self.execute_installed(&mut invocation)
    }

    fn execute_shell_command(
        &mut self,
        command: &str,
        arguments: &[String],
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        _sink: &mut dyn EventSink,
    ) -> CommandOutput {
        if arguments.first().map(String::as_str) != Some("-c")
            || arguments.get(1).is_none()
            || arguments.len() > MAX_SOURCE_ARGUMENTS + 3
        {
            return usage(
                command,
                &format!("usage: {command} -c SCRIPT [NAME [ARG ...]]"),
            );
        }
        if source_depth >= MAX_SOURCE_DEPTH {
            return CommandOutput::failure(
                2,
                format!("{command}: shell nesting exceeds the {MAX_SOURCE_DEPTH}-level limit\n"),
            );
        }
        let script = &arguments[1];
        let name = arguments
            .get(2)
            .cloned()
            .unwrap_or_else(|| command.to_string());
        let mut parameters = Vec::with_capacity(arguments.len() - 1);
        parameters.push(name);
        parameters.extend(arguments.iter().skip(3).cloned());
        let previous_parameters = std::mem::replace(&mut self.script_parameters, parameters);
        let previous_loop_depth = self.loop_depth;
        let previous_loop_control = self.loop_control.take();
        let previous_functions = std::mem::take(&mut self.functions);
        let previous_function_depth = self.function_depth;
        let previous_function_return = self.function_return.take();
        let previous_local_bindings = std::mem::take(&mut self.function_local_bindings);
        self.loop_depth = 0;
        self.function_depth = 0;
        self.function_return = None;
        self.function_local_bindings = Vec::new();
        // `sh -c` is itself a command in the caller's pipeline. Let the
        // caller emit its aggregate result so nested lines cannot be
        // delivered a second time through the same event stream.
        let mut nested_sink = NoopEventSink;
        let output = self.execute_script_internal(
            script,
            record_history,
            source_depth + 1,
            external_stdin,
            &mut nested_sink,
        );
        self.script_parameters = previous_parameters;
        self.loop_depth = previous_loop_depth;
        self.loop_control = previous_loop_control;
        self.functions = previous_functions;
        self.function_depth = previous_function_depth;
        self.function_return = previous_function_return;
        self.function_local_bindings = previous_local_bindings;
        output
    }

    fn execute_xargs(
        &mut self,
        arguments: &[String],
        stdin: &str,
        source_depth: usize,
        _sink: &mut dyn EventSink,
    ) -> CommandOutput {
        let plan = match commands::parse_xargs_plan(arguments, stdin) {
            Ok(plan) => plan,
            Err(error) => return usage("xargs", &error),
        };
        let mut output = CommandOutput::success("");
        let mut inner_sink = NoopEventSink;
        for batch in &plan.batches {
            if let Some(cancellation) = self.take_cancellation() {
                output.stderr.push_str(&cancellation.stderr);
                output.status = cancellation.status;
                break;
            }
            let line = commands::xargs_command_line(&plan, batch);
            let parsed = match parse(&line) {
                Ok(parsed) => parsed,
                Err(error) => {
                    output.status = 2;
                    let _ = writeln!(output.stderr, "xargs: generated command: {error}");
                    break;
                }
            };
            let invocation = self.execute_plan(&parsed, false, source_depth, "", &mut inner_sink);
            output.stdout.push_str(&invocation.stdout);
            output.stderr.push_str(&invocation.stderr);
            output.status = invocation.status;
            limit_output(&mut output);
        }
        output
    }

    fn expand_word(
        &mut self,
        word: &Word,
        source_depth: usize,
    ) -> Result<ExpandedWord, CommandOutput> {
        let environment = self.environment.clone();
        let last_status = self.last_status;
        let script_parameters = self.script_parameters.clone();
        let mut value = String::new();
        let mut stderr = String::new();
        let mut has_wildcard = false;
        for part in word.parts() {
            match part {
                WordPart::Literal(text) => value.push_str(text),
                WordPart::Variable(name) if name == "?" => value.push_str(&last_status.to_string()),
                WordPart::Variable(name) if name == "#" => {
                    value.push_str(&script_parameters.len().saturating_sub(1).to_string());
                }
                WordPart::Variable(name) if name == "@" => {
                    if script_parameters.len() > 1 {
                        value.push_str(&script_parameters[1..].join(" "));
                    }
                }
                WordPart::Variable(name) if let Ok(index) = name.parse::<usize>() => {
                    if let Some(parameter) = script_parameters.get(index) {
                        value.push_str(parameter);
                    }
                }
                WordPart::Variable(name) => {
                    if let Some(variable_value) = environment.get(name) {
                        value.push_str(variable_value);
                    }
                }
                WordPart::CommandSubstitution(command) => {
                    let output = self.execute_command_substitution(command, source_depth)?;
                    value.push_str(output.stdout.trim_end_matches('\n'));
                    stderr.push_str(&output.stderr);
                }
                WordPart::Wildcard(wildcard) => {
                    value.push(*wildcard);
                    has_wildcard = true;
                }
            }
        }
        Ok(ExpandedWord {
            value,
            has_wildcard,
            stderr,
        })
    }

    fn execute_command_substitution(
        &mut self,
        command: &str,
        source_depth: usize,
    ) -> Result<CommandOutput, CommandOutput> {
        if command.len() > MAX_COMMAND_INPUT_BYTES {
            return Err(CommandOutput::failure(
                2,
                format!(
                    "rune: command substitution exceeds the {MAX_COMMAND_INPUT_BYTES}-byte input limit\n"
                ),
            ));
        }
        if self.command_substitution_depth >= MAX_COMMAND_SUBSTITUTION_DEPTH {
            return Err(CommandOutput::failure(
                2,
                format!(
                    "rune: command substitution nesting exceeds the {MAX_COMMAND_SUBSTITUTION_DEPTH}-level limit\n"
                ),
            ));
        }
        let plan = parse(command).map_err(|error| {
            CommandOutput::failure(2, format!("rune: command substitution: {error}\n"))
        })?;
        if plan.is_empty() {
            return Ok(CommandOutput::success(""));
        }

        let previous_directory = self.filesystem.current_dir_display();
        let previous_environment = self.environment.clone();
        let previous_aliases = self.aliases.clone();
        let previous_bookmarks = self.bookmarks.clone();
        let previous_directory_usage = self.directory_usage.clone();
        let previous_config = self.config.clone();
        let previous_history = self.history.clone();
        let previous_history_limit = self.history_limit;
        let previous_status = self.last_status;
        let previous_parameters = self.script_parameters.clone();
        let previous_depth = self.command_substitution_depth;
        let previous_loop_depth = self.loop_depth;
        let previous_loop_control = self.loop_control.take();
        let previous_functions = std::mem::take(&mut self.functions);
        let previous_function_depth = self.function_depth;
        let previous_function_return = self.function_return.take();
        let previous_local_bindings = std::mem::take(&mut self.function_local_bindings);

        self.command_substitution_depth += 1;
        self.loop_depth = 0;
        self.function_depth = 0;
        self.function_return = None;
        self.function_local_bindings = Vec::new();
        let mut sink = NoopEventSink;
        let mut output = self.execute_plan(&plan, false, source_depth, "", &mut sink);
        self.command_substitution_depth = previous_depth;
        self.loop_depth = previous_loop_depth;
        self.loop_control = previous_loop_control;
        self.functions = previous_functions;
        self.function_depth = previous_function_depth;
        self.function_return = previous_function_return;
        self.function_local_bindings = previous_local_bindings;
        let _ = self.filesystem.change_dir(&previous_directory);
        self.environment = previous_environment;
        self.aliases = previous_aliases;
        self.bookmarks = previous_bookmarks;
        self.directory_usage = previous_directory_usage;
        self.config = previous_config;
        self.history = previous_history;
        self.history_limit = previous_history_limit;
        self.last_status = previous_status;
        self.script_parameters = previous_parameters;
        output.stdout = output.stdout.trim_end_matches('\n').to_string();
        limit_output(&mut output);
        Ok(output)
    }

    fn expand_command_words(
        &mut self,
        command: &CommandPlan,
        source_depth: usize,
    ) -> Result<(String, Vec<String>, String), CommandOutput> {
        let program_expanded = self.expand_word(&command.program, source_depth)?;
        let program = program_expanded.value;
        let mut stderr = program_expanded.stderr;
        let mut arguments = Vec::new();
        for word in &command.arguments {
            let expanded = self.expand_word(word, source_depth)?;
            stderr.push_str(&expanded.stderr);
            if expanded.has_wildcard {
                match self.filesystem.glob(&expanded.value) {
                    Ok(matches) => arguments.extend(matches),
                    Err(error) => return Err(fs_failure(&program, &error)),
                }
            } else {
                arguments.push(expanded.value);
            }
        }
        Ok((program, arguments, stderr))
    }

    fn execute_source(
        &mut self,
        command: &str,
        arguments: &[String],
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        _sink: &mut dyn EventSink,
    ) -> CommandOutput {
        if arguments.is_empty() || arguments.len() > MAX_SOURCE_ARGUMENTS + 1 {
            return usage(
                command,
                &format!(
                    "usage: {command} FILE [ARG ...] (up to {MAX_SOURCE_ARGUMENTS} arguments)"
                ),
            );
        }
        if source_depth >= MAX_SOURCE_DEPTH {
            return CommandOutput::failure(
                2,
                format!("{command}: source nesting exceeds the {MAX_SOURCE_DEPTH}-level limit\n"),
            );
        }
        let path = &arguments[0];
        let bytes = match self.filesystem.read(path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure(command, &error),
        };
        if bytes.len() > MAX_SCRIPT_BYTES {
            return CommandOutput::failure(
                2,
                format!("{command}: file exceeds the {MAX_SCRIPT_BYTES}-byte input limit\n"),
            );
        }
        let Ok(script) = String::from_utf8(bytes) else {
            return CommandOutput::failure(2, format!("{command}: file is not valid UTF-8\n"));
        };
        let previous_parameters =
            std::mem::replace(&mut self.script_parameters, arguments.to_vec());
        // `source` is itself a command in the caller's pipeline. Let the
        // caller emit its aggregate result so sourced lines cannot be
        // delivered a second time through the same event stream.
        let mut nested_sink = NoopEventSink;
        let output = self.execute_script_internal(
            &script,
            record_history,
            source_depth + 1,
            external_stdin,
            &mut nested_sink,
        );
        self.script_parameters = previous_parameters;
        output
    }

    fn apply_redirections(
        &mut self,
        command: &CommandPlan,
        program: &str,
        external_stdin: &str,
        source_depth: usize,
    ) -> Result<(AppliedRedirections, String), CommandOutput> {
        let mut stdin = external_stdin.to_string();
        let mut stdout = OutputTarget::Stdout;
        let mut stderr = OutputTarget::Stderr;
        let mut expansion_stderr = String::new();
        for (descriptor, redirection) in command.redirections.iter().enumerate() {
            match redirection {
                Redirection::Stdin { path } => {
                    let expanded = self.expand_word(path, source_depth)?;
                    expansion_stderr.push_str(&expanded.stderr);
                    let path = expanded.value;
                    match self.filesystem.read(&path) {
                        Ok(content) => stdin = String::from_utf8_lossy(&content).into_owned(),
                        Err(error) => return Err(fs_failure(program, &error)),
                    }
                }
                Redirection::Stdout { path, append } => {
                    let expanded = self.expand_word(path, source_depth)?;
                    expansion_stderr.push_str(&expanded.stderr);
                    let path = expanded.value;
                    if let Err(error) = self.filesystem.write(&path, &[], *append) {
                        return Err(fs_failure(program, &error));
                    }
                    stdout = OutputTarget::File {
                        path,
                        append: *append,
                        descriptor,
                    };
                }
                Redirection::Stderr { path, append } => {
                    let expanded = self.expand_word(path, source_depth)?;
                    expansion_stderr.push_str(&expanded.stderr);
                    let path = expanded.value;
                    if let Err(error) = self.filesystem.write(&path, &[], *append) {
                        return Err(fs_failure(program, &error));
                    }
                    stderr = OutputTarget::File {
                        path,
                        append: *append,
                        descriptor,
                    };
                }
                Redirection::Both { path, append } => {
                    let expanded = self.expand_word(path, source_depth)?;
                    expansion_stderr.push_str(&expanded.stderr);
                    let path = expanded.value;
                    if let Err(error) = self.filesystem.write(&path, &[], *append) {
                        return Err(fs_failure(program, &error));
                    }
                    let target = OutputTarget::File {
                        path,
                        append: *append,
                        descriptor,
                    };
                    stdout = target.clone();
                    stderr = target;
                }
                Redirection::StdoutToStderr => stdout = stderr.clone(),
                Redirection::StderrToStdout => stderr = stdout.clone(),
            }
        }
        Ok((
            AppliedRedirections {
                stdin,
                stdout,
                stderr,
            },
            expansion_stderr,
        ))
    }

    fn apply_output_redirections(
        &mut self,
        program: &str,
        output: &mut CommandOutput,
        redirections: AppliedRedirections,
    ) {
        let stdout = std::mem::take(&mut output.stdout);
        let stderr = std::mem::take(&mut output.stderr);
        if redirections.stdout == redirections.stderr {
            let mut combined = stdout;
            combined.push_str(&stderr);
            self.route_output(program, output, redirections.stdout, &combined);
        } else {
            self.route_output(program, output, redirections.stdout, &stdout);
            self.route_output(program, output, redirections.stderr, &stderr);
        }
    }

    fn route_output(
        &mut self,
        program: &str,
        output: &mut CommandOutput,
        target: OutputTarget,
        content: &str,
    ) {
        match target {
            OutputTarget::Stdout => output.stdout.push_str(content),
            OutputTarget::Stderr => output.stderr.push_str(content),
            OutputTarget::File {
                path,
                append,
                descriptor: _,
            } => {
                if let Err(error) = self.filesystem.write(&path, content.as_bytes(), append) {
                    output.status = 1;
                    let _ = writeln!(output.stderr, "{program}: {error}");
                }
            }
        }
    }

    fn apply_history_limit(&mut self) {
        self.history_limit = self.config.history_limit();
        persistence::apply_history_limit(&mut self.history, self.history_limit);
    }

    fn take_cancellation(&self) -> Option<CommandOutput> {
        self.cancellation_requested
            .swap(false, Ordering::AcqRel)
            .then(|| CommandOutput::failure(CANCELLED_STATUS, "rune: command cancelled\n"))
    }

    fn find_installed_command(&self, name: &str) -> Result<Option<InstalledCommand>, FsError> {
        find_installed_command_in_filesystem(self.filesystem.as_ref(), name)
    }

    fn execute_installed_wasm(
        &mut self,
        program: &str,
        arguments: &[String],
        stdin: &str,
        installed_command: &InstalledCommand,
    ) -> CommandOutput {
        if !is_wasm_entry(&installed_command.entry) {
            return CommandOutput::failure(
                126,
                format!(
                    "{program}: package {} exposes unsupported entry {}; only WASM, Python, Lua, JavaScript, and Rune scripts are available\n",
                    installed_command.package, installed_command.entry
                ),
            );
        }
        let module = match self.verified_installed_entry(program, installed_command) {
            Ok(module) => module,
            Err(output) => return output,
        };
        let host_preopens = self.filesystem.host_preopens();
        let runtime_preopens = host_preopens
            .iter()
            .skip(1)
            .map(|(host_path, guest_path)| rune_runtime::RuntimePreopen::new(host_path, guest_path))
            .collect::<Vec<_>>();
        let request = RuntimeRequest::new(
            RuntimeKind::Wasm,
            program,
            &module,
            arguments,
            &self.environment,
            stdin,
        )
        .with_preopened_root(
            installed_command
                .filesystem_access
                .then(|| host_preopens.first().map(|(host_path, _)| *host_path))
                .flatten(),
        )
        .with_additional_preopens(if installed_command.filesystem_access {
            runtime_preopens.as_slice()
        } else {
            &[]
        })
        .with_cancellation(Some(&self.cancellation_requested));
        match Runtime::execute(&self.wasm_runner, &request) {
            Ok(execution) => CommandOutput {
                stdout: execution.stdout,
                stderr: execution.stderr,
                status: execution.status,
            },
            Err(error) => CommandOutput::failure(
                126,
                format!("{program}: installed package runtime failed: {error}\n"),
            ),
        }
    }

    fn execute_installed_lua(
        &mut self,
        program: &str,
        arguments: &[String],
        stdin: &str,
        installed_command: &InstalledCommand,
    ) -> CommandOutput {
        let source = match self.verified_installed_entry(program, installed_command) {
            Ok(source) => source,
            Err(output) => return output,
        };
        let request = RuntimeRequest::new(
            RuntimeKind::Lua,
            program,
            &source,
            arguments,
            &self.environment,
            stdin,
        )
        .with_cancellation(Some(&self.cancellation_requested));
        match Runtime::execute(&self.lua_runner, &request) {
            Ok(execution) => CommandOutput {
                stdout: execution.stdout,
                stderr: execution.stderr,
                status: execution.status,
            },
            Err(error) => CommandOutput::failure(
                126,
                format!("{program}: installed package Lua runtime failed: {error}\n"),
            ),
        }
    }

    fn execute_installed_python(
        &mut self,
        program: &str,
        arguments: &[String],
        stdin: &str,
        installed_command: &InstalledCommand,
    ) -> CommandOutput {
        let source = match self.verified_installed_entry(program, installed_command) {
            Ok(source) => source,
            Err(output) => return output,
        };
        let request = RuntimeRequest::new(
            RuntimeKind::Python,
            program,
            &source,
            arguments,
            &self.environment,
            stdin,
        )
        .with_cancellation(Some(&self.cancellation_requested));
        match Runtime::execute(&self.python_runner, &request) {
            Ok(execution) => CommandOutput {
                stdout: execution.stdout,
                stderr: execution.stderr,
                status: execution.status,
            },
            Err(error) => CommandOutput::failure(
                126,
                format!("{program}: installed package Python runtime failed: {error}\n"),
            ),
        }
    }

    fn execute_installed_javascript(
        &mut self,
        program: &str,
        arguments: &[String],
        stdin: &str,
        installed_command: &InstalledCommand,
    ) -> CommandOutput {
        let source = match self.verified_installed_entry(program, installed_command) {
            Ok(source) => source,
            Err(output) => return output,
        };
        let request = RuntimeRequest::new(
            RuntimeKind::JavaScript,
            program,
            &source,
            arguments,
            &self.environment,
            stdin,
        )
        .with_cancellation(Some(&self.cancellation_requested));
        match Runtime::execute(&self.javascript_runner, &request) {
            Ok(execution) => CommandOutput {
                stdout: execution.stdout,
                stderr: execution.stderr,
                status: execution.status,
            },
            Err(error) => CommandOutput::failure(
                126,
                format!("{program}: installed package JavaScript runtime failed: {error}\n"),
            ),
        }
    }

    fn execute_installed(&mut self, invocation: &mut InstalledInvocation<'_>) -> CommandOutput {
        if is_rune_script_entry(invocation.command.entry.as_str()) {
            self.execute_installed_script(invocation)
        } else if is_python_entry(invocation.command.entry.as_str()) {
            self.execute_installed_python(
                invocation.program,
                invocation.arguments,
                invocation.stdin,
                invocation.command,
            )
        } else if is_lua_entry(invocation.command.entry.as_str()) {
            self.execute_installed_lua(
                invocation.program,
                invocation.arguments,
                invocation.stdin,
                invocation.command,
            )
        } else if is_javascript_entry(invocation.command.entry.as_str()) {
            self.execute_installed_javascript(
                invocation.program,
                invocation.arguments,
                invocation.stdin,
                invocation.command,
            )
        } else {
            self.execute_installed_wasm(
                invocation.program,
                invocation.arguments,
                invocation.stdin,
                invocation.command,
            )
        }
    }

    fn execute_installed_script(
        &mut self,
        invocation: &mut InstalledInvocation<'_>,
    ) -> CommandOutput {
        if invocation.source_depth >= MAX_SOURCE_DEPTH {
            return CommandOutput::failure(
                2,
                format!(
                    "{}: package script nesting exceeds the {MAX_SOURCE_DEPTH}-level limit\n",
                    invocation.program
                ),
            );
        }
        let module = match self.verified_installed_entry(invocation.program, invocation.command) {
            Ok(module) => module,
            Err(output) => return output,
        };
        let Ok(script) = String::from_utf8(module) else {
            return CommandOutput::failure(
                126,
                format!(
                    "{}: installed package script is not valid UTF-8\n",
                    invocation.program
                ),
            );
        };
        let mut parameters = Vec::with_capacity(invocation.arguments.len() + 1);
        parameters.push(invocation.program.to_string());
        parameters.extend(invocation.arguments.iter().cloned());
        let previous_parameters = std::mem::replace(&mut self.script_parameters, parameters);
        let output = self.execute_script_internal(
            &script,
            invocation.record_history,
            invocation.source_depth + 1,
            invocation.stdin,
            invocation.sink,
        );
        self.script_parameters = previous_parameters;
        output
    }

    fn verified_installed_entry(
        &self,
        program: &str,
        installed_command: &InstalledCommand,
    ) -> Result<Vec<u8>, CommandOutput> {
        let module = self
            .filesystem
            .read(&installed_command.module_path)
            .map_err(|error| fs_failure(program, &error))?;
        let manifest = self
            .filesystem
            .read(&installed_command.manifest_path)
            .map_err(|error| fs_failure(program, &error))
            .and_then(|bytes| {
                PackageManifest::parse(&bytes)
                    .map_err(|error| package_runtime_failure(program, &error))
            })?;
        manifest
            .verify_file(&installed_command.entry, &module)
            .map_err(|error| package_runtime_failure(program, &error))?;
        Ok(module)
    }

    fn expand_alias(
        &self,
        command: &CommandPlan,
        depth: usize,
    ) -> Result<Option<CommandPlan>, String> {
        let Some(name) = command.program.literal_value() else {
            return Ok(None);
        };
        let Some(alias) = self.aliases.get(&name) else {
            return Ok(None);
        };
        if depth >= MAX_ALIAS_EXPANSIONS {
            return Err(format!(
                "alias expansion exceeded {MAX_ALIAS_EXPANSIONS} levels"
            ));
        }

        let plan = parse(alias).map_err(|error| format!("{name}: {error}"))?;
        if plan.pipelines.len() != 1 || !plan.connectors.is_empty() {
            return Err(format!("{name}: only one command is allowed in an alias"));
        }
        let Some(pipeline) = plan.pipelines.into_iter().next() else {
            return Err(format!("{name}: alias value is empty"));
        };
        if pipeline.commands.len() != 1 {
            return Err(format!("{name}: only one command is allowed in an alias"));
        }
        let Some(mut replacement) = pipeline.commands.into_iter().next() else {
            return Err(format!("{name}: alias value is empty"));
        };
        if replacement.program.parts().is_empty() {
            return Err(format!("{name}: alias value must contain one command"));
        }

        let mut assignments = command.assignments.clone();
        assignments.append(&mut replacement.assignments);
        let mut arguments = replacement.arguments;
        arguments.extend(command.arguments.clone());
        let mut redirections = replacement.redirections;
        redirections.extend(command.redirections.clone());
        replacement.assignments = assignments;
        replacement.arguments = arguments;
        replacement.redirections = redirections;
        Ok(Some(replacement))
    }

    fn update_pwd(&mut self) {
        self.environment
            .insert("PWD".to_string(), self.filesystem.current_dir_display());
    }

    fn update_directory_environment(&mut self, previous_directory: String) {
        let current_directory = self.filesystem.current_dir_display();
        if current_directory != previous_directory {
            self.environment
                .insert("OLDPWD".to_string(), previous_directory);
            self.record_directory_usage(current_directory.clone());
        }
        self.update_pwd();
    }

    fn record_directory_usage(&mut self, directory: String) {
        if directory.len() > MAX_BOOKMARK_PATH_BYTES {
            return;
        }
        if let Some(count) = self.directory_usage.get_mut(&directory) {
            *count = count.saturating_add(1);
            return;
        }
        if self.directory_usage.len() >= MAX_DIRECTORY_USAGE_ENTRIES {
            let least_used = self
                .directory_usage
                .iter()
                .min_by(|(left_path, left_count), (right_path, right_count)| {
                    left_count
                        .cmp(right_count)
                        .then_with(|| left_path.cmp(right_path))
                })
                .map(|(path, _)| path.clone());
            if let Some(path) = least_used {
                self.directory_usage.remove(&path);
            }
        }
        self.directory_usage.insert(directory, 1);
    }
}

fn find_installed_command_in_filesystem(
    filesystem: &dyn VirtualFileSystem,
    name: &str,
) -> Result<Option<InstalledCommand>, FsError> {
    Ok(installed_commands_in_filesystem(filesystem)?
        .into_iter()
        .find(|command| command.name == name))
}

fn installed_commands_in_filesystem(
    filesystem: &dyn VirtualFileSystem,
) -> Result<Vec<InstalledCommand>, FsError> {
    let packages = match filesystem.list(Some(PACKAGE_INSTALL_ROOT)) {
        Ok(entries) => entries,
        Err(FsError::NotFound(_)) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut commands = Vec::new();
    for package in packages.into_iter().filter(|entry| entry.is_directory) {
        let package_path = format!("{PACKAGE_INSTALL_ROOT}/{}", package.name);
        let Ok(versions) = filesystem.list(Some(&package_path)) else {
            continue;
        };
        for version in versions.into_iter().filter(|entry| entry.is_directory) {
            let version_path = format!("{package_path}/{}", version.name);
            let manifest_path = format!("{version_path}/manifest.json");
            let Ok(bytes) = filesystem.read(&manifest_path) else {
                continue;
            };
            let Ok(manifest) = PackageManifest::parse(&bytes) else {
                continue;
            };
            for command in manifest.commands {
                commands.push(InstalledCommand {
                    name: command.name,
                    package: format!("{}@{}", manifest.name, manifest.version),
                    manifest_path: manifest_path.clone(),
                    module_path: format!("{version_path}/{}", command.entry),
                    entry: command.entry,
                    filesystem_access: manifest.permissions.filesystem,
                });
                if commands.len() >= MAX_INSTALLED_COMMANDS {
                    return Ok(commands);
                }
            }
        }
    }
    Ok(commands)
}

#[derive(Debug)]
struct ForHeader {
    variable: String,
    values: Vec<Word>,
    inline_do: bool,
}

#[derive(Debug)]
struct IfHeader {
    condition: String,
    inline_then: bool,
}

#[derive(Debug)]
struct IfClause {
    condition: Option<String>,
    body: String,
}

#[derive(Debug)]
struct IfBlock {
    clauses: Vec<IfClause>,
    end: usize,
}

#[derive(Debug)]
struct WhileHeader {
    condition: String,
    inline_do: bool,
    until: bool,
}

#[derive(Debug)]
struct CaseHeader {
    word: Word,
}

#[derive(Debug)]
struct CaseClause {
    patterns: Vec<CasePattern>,
    body: String,
}

#[derive(Debug)]
struct CaseBlock {
    word: Word,
    clauses: Vec<CaseClause>,
    end: usize,
}

#[derive(Debug)]
struct CasePattern {
    tokens: Vec<CasePatternToken>,
}

#[derive(Debug)]
enum CasePatternToken {
    Literal(char),
    AnySequence,
    AnyCharacter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlBlock {
    Loop,
    If,
    Case,
    Function,
}

fn is_for_header_line(line: &str) -> bool {
    line.trim_start().starts_with("for ")
}

fn normalized_script_lines(script: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in script.lines() {
        if let Some((header, body)) = split_inline_loop_line(line) {
            lines.push(header);
            lines.push(body);
            lines.push("done".to_string());
        } else if let Some((header, body)) = split_inline_if_line(line) {
            lines.push(header);
            lines.push(body);
            lines.push("fi".to_string());
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

fn split_inline_loop_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    let do_marker = find_unquoted_marker(trimmed, "; do", 0)?;
    let body_start = do_marker + "; do".len();
    let done_marker = find_unquoted_marker(trimmed, "; done", body_start)?;
    if !trimmed[done_marker + "; done".len()..].trim().is_empty() {
        return None;
    }
    let header = trimmed[..do_marker].trim();
    if !(is_for_header_line(header) || is_loop_header_line(header)) {
        return None;
    }
    let body = trimmed[body_start..done_marker].trim();
    if body.is_empty() {
        return None;
    }
    Some((format!("{header}; do"), body.to_string()))
}

fn split_inline_if_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    let then_marker = find_unquoted_marker(trimmed, "; then", 0)?;
    let body_start = then_marker + "; then".len();
    let fi_marker = find_unquoted_marker(trimmed, "; fi", body_start)?;
    if !trimmed[fi_marker + "; fi".len()..].trim().is_empty() {
        return None;
    }
    let header = trimmed[..then_marker].trim();
    if !is_if_header_line(header) {
        return None;
    }
    let body = trimmed[body_start..fi_marker].trim();
    if body.is_empty() || find_unquoted_marker(body, "; else", 0).is_some() {
        return None;
    }
    Some((format!("{header}; then"), body.to_string()))
}

fn find_unquoted_marker(input: &str, marker: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in input.char_indices().filter(|(index, _)| *index >= start) {
        if escaped {
            escaped = false;
            continue;
        }
        if quote == Some('"') && character == '\\' {
            escaped = true;
            continue;
        }
        match quote {
            Some(expected) if character == expected => quote = None,
            None if character == '\'' || character == '"' => quote = Some(character),
            None if input[index..].starts_with(marker) => return Some(index),
            Some(_) | None => {}
        }
    }
    None
}

fn is_if_header_line(line: &str) -> bool {
    line.trim_start().starts_with("if ")
}

fn is_loop_header_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("while ") || trimmed.starts_with("until ")
}

fn is_case_header_line(line: &str) -> bool {
    line.trim_start().starts_with("case ")
}

fn is_function_header_line(line: &str) -> bool {
    line.trim().ends_with("() {")
}

fn parse_inline_function_definition(line: &str) -> Result<Option<(String, String)>, String> {
    let trimmed = line.trim();
    let Some((raw_name, body)) = trimmed.split_once("() {") else {
        return Ok(None);
    };
    if raw_name
        .chars()
        .any(|character| character.is_whitespace() || matches!(character, '\'' | '"'))
    {
        return Ok(None);
    }
    let Some(body) = body.strip_suffix('}') else {
        return Ok(None);
    };
    let name = parse_function_name(raw_name)?;
    Ok(Some((name, body.trim().to_string())))
}

fn parse_function_header(line: &str) -> Result<Option<String>, String> {
    let trimmed = line.trim();
    let Some(name) = trimmed.strip_suffix("() {") else {
        return Ok(None);
    };
    Ok(Some(parse_function_name(name)?))
}

fn parse_function_name(raw_name: &str) -> Result<String, String> {
    let name = raw_name.trim();
    if !is_valid_script_variable(name) {
        return Err(format!("invalid function name: {name}"));
    }
    if name.len() > MAX_FUNCTION_NAME_BYTES {
        return Err(format!(
            "function name exceeds the {MAX_FUNCTION_NAME_BYTES}-byte limit"
        ));
    }
    Ok(name.to_string())
}

fn is_case_clause_header(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed.ends_with(')')
        && trimmed != ")"
        && trimmed != "esac"
        && trimmed != ";;"
}

fn is_elif_header_line(line: &str) -> bool {
    line.trim_start().starts_with("elif ")
}

fn parse_for_header(line: &str) -> Result<Option<ForHeader>, String> {
    let trimmed = line.trim();
    let Some(rest) = trimmed.strip_prefix("for ") else {
        return Ok(None);
    };
    let rest = rest.trim_end();
    let (header, inline_do) = rest
        .strip_suffix("; do")
        .map_or((rest, false), |header| (header.trim_end(), true));
    let plan =
        parse(&format!("for {header}")).map_err(|error| format!("invalid for header: {error}"))?;
    let Some(pipeline) = plan.pipelines.first() else {
        return Err("for loop header is empty".to_string());
    };
    let Some(command) = pipeline.commands.first() else {
        return Err("for loop header has no command".to_string());
    };
    if plan.pipelines.len() != 1
        || !plan.connectors.is_empty()
        || pipeline.commands.len() != 1
        || !command.assignments.is_empty()
        || !command.redirections.is_empty()
        || command.program.literal_value().as_deref() != Some("for")
    {
        return Err("use: for NAME in VALUE ...; do".to_string());
    }
    let Some(variable) = command.arguments.first().and_then(Word::literal_value) else {
        return Err("for loop variable is missing".to_string());
    };
    if !is_valid_script_variable(&variable) {
        return Err(format!("invalid for loop variable: {variable}"));
    }
    if command
        .arguments
        .get(1)
        .and_then(Word::literal_value)
        .as_deref()
        != Some("in")
    {
        return Err("for loop header requires `in`".to_string());
    }
    Ok(Some(ForHeader {
        variable,
        values: command.arguments.iter().skip(2).cloned().collect(),
        inline_do,
    }))
}

fn parse_while_header(line: &str) -> Result<Option<WhileHeader>, String> {
    let trimmed = line.trim();
    let (keyword, until) = if trimmed.starts_with("while ") {
        ("while", false)
    } else if trimmed.starts_with("until ") {
        ("until", true)
    } else {
        return Ok(None);
    };
    let Some(rest) = trimmed.strip_prefix(&format!("{keyword} ")) else {
        return Err(format!("invalid {keyword} header"));
    };
    let rest = rest.trim_end();
    let (condition, inline_do) = rest
        .strip_suffix("; do")
        .map_or((rest, false), |condition| (condition.trim_end(), true));
    if condition.is_empty() {
        return Err(format!("{keyword} condition is empty"));
    }
    parse(condition).map_err(|error| format!("invalid {keyword} condition: {error}"))?;
    Ok(Some(WhileHeader {
        condition: condition.to_string(),
        inline_do,
        until,
    }))
}

fn parse_case_header(line: &str) -> Result<Option<CaseHeader>, String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("case ") {
        return Ok(None);
    }
    let plan = parse(trimmed).map_err(|error| format!("invalid case header: {error}"))?;
    let Some(pipeline) = plan.pipelines.first() else {
        return Err("case header is empty".to_string());
    };
    let Some(command) = pipeline.commands.first() else {
        return Err("case header has no command".to_string());
    };
    if plan.pipelines.len() != 1
        || !plan.connectors.is_empty()
        || pipeline.commands.len() != 1
        || !command.assignments.is_empty()
        || !command.redirections.is_empty()
        || command.program.literal_value().as_deref() != Some("case")
        || command.arguments.len() != 2
        || command.arguments[1].literal_value().as_deref() != Some("in")
    {
        return Err("use: case WORD in".to_string());
    }
    let word = command
        .arguments
        .first()
        .cloned()
        .ok_or_else(|| "case word is missing".to_string())?;
    Ok(Some(CaseHeader { word }))
}

fn parse_if_header(line: &str, keyword: &str) -> Result<Option<IfHeader>, String> {
    let trimmed = line.trim();
    let Some(rest) = trimmed.strip_prefix(&format!("{keyword} ")) else {
        return Ok(None);
    };
    let rest = rest.trim_end();
    let (condition, inline_then) = rest
        .strip_suffix("; then")
        .map_or((rest, false), |condition| (condition.trim_end(), true));
    if condition.is_empty() {
        return Err(format!("{keyword} condition is empty"));
    }
    parse(condition).map_err(|error| format!("invalid {keyword} condition: {error}"))?;
    Ok(Some(IfHeader {
        condition: condition.to_string(),
        inline_then,
    }))
}

fn is_valid_script_variable(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(
        characters.next(),
        Some(character) if character == '_' || character.is_ascii_alphabetic()
    ) && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn function_body_range(lines: &[&str], header_index: usize) -> Result<(usize, usize), String> {
    let body_start = header_index + 1;
    let mut blocks = vec![ControlBlock::Function];
    for (index, line) in lines.iter().enumerate().skip(body_start) {
        if is_function_header_line(line) {
            blocks.push(ControlBlock::Function);
        } else if is_for_header_line(line) || is_loop_header_line(line) {
            blocks.push(ControlBlock::Loop);
        } else if is_if_header_line(line) {
            blocks.push(ControlBlock::If);
        } else if is_case_header_line(line) {
            blocks.push(ControlBlock::Case);
        } else {
            match line.trim() {
                "done" => match blocks.pop() {
                    Some(ControlBlock::Loop) => {}
                    _ => return Err("function has mismatched control markers".to_string()),
                },
                "fi" if blocks.pop() != Some(ControlBlock::If) => {
                    return Err("function has mismatched control markers".to_string());
                }
                "esac" if blocks.pop() != Some(ControlBlock::Case) => {
                    return Err("function has mismatched control markers".to_string());
                }
                "}" => match blocks.pop() {
                    Some(ControlBlock::Function) if blocks.is_empty() => {
                        return Ok((body_start, index));
                    }
                    Some(ControlBlock::Function) => {}
                    _ => return Err("function has mismatched control markers".to_string()),
                },
                _ => {}
            }
        }
    }
    Err("function is missing `}`".to_string())
}

fn loop_body_range(
    lines: &[&str],
    header_index: usize,
    inline_do: bool,
) -> Result<(usize, usize), String> {
    let mut body_start = header_index + 1;
    if !inline_do {
        if lines.get(body_start).map(|line| line.trim()) != Some("do") {
            return Err("for loop is missing `do`".to_string());
        }
        body_start += 1;
    }
    let mut blocks = vec![ControlBlock::Loop];
    for (index, line) in lines.iter().enumerate().skip(body_start) {
        if is_function_header_line(line) {
            blocks.push(ControlBlock::Function);
        } else if is_for_header_line(line) || is_loop_header_line(line) {
            blocks.push(ControlBlock::Loop);
        } else if is_if_header_line(line) {
            blocks.push(ControlBlock::If);
        } else if is_case_header_line(line) {
            blocks.push(ControlBlock::Case);
        } else {
            match line.trim() {
                "done" => match blocks.pop() {
                    Some(ControlBlock::Loop) if blocks.is_empty() => {
                        return Ok((body_start, index));
                    }
                    Some(ControlBlock::Loop) => {}
                    _ => return Err("for loop has mismatched `done`/`fi`".to_string()),
                },
                "fi" if blocks.pop() != Some(ControlBlock::If) => {
                    return Err("for loop has mismatched `done`/`fi`".to_string());
                }
                "esac" if blocks.pop() != Some(ControlBlock::Case) => {
                    return Err("for loop has mismatched `esac`".to_string());
                }
                "}" if blocks.pop() != Some(ControlBlock::Function) => {
                    return Err("for loop has mismatched `}`".to_string());
                }
                _ => {}
            }
        }
    }
    Err("for loop is missing `done`".to_string())
}

fn parse_case_block(
    lines: &[&str],
    header_index: usize,
    header: &CaseHeader,
) -> Result<CaseBlock, String> {
    let mut clauses = Vec::new();
    let mut current_patterns = None;
    let mut current_start = 0;
    let mut index = header_index + 1;
    while index < lines.len() {
        let line = lines[index];
        if is_for_header_line(line) {
            let nested =
                parse_for_header(line)?.ok_or_else(|| "invalid nested `for` header".to_string())?;
            let (_, body_end) = loop_body_range(lines, index, nested.inline_do)?;
            index = body_end + 1;
            continue;
        }
        if is_loop_header_line(line) {
            let nested = parse_while_header(line)?
                .ok_or_else(|| "invalid nested loop header".to_string())?;
            let (_, body_end) = loop_body_range(lines, index, nested.inline_do)?;
            index = body_end + 1;
            continue;
        }
        if is_if_header_line(line) {
            let nested = parse_if_header(line, "if")?
                .ok_or_else(|| "invalid nested `if` header".to_string())?;
            let nested_block = parse_if_block(lines, index, &nested)?;
            index = nested_block.end + 1;
            continue;
        }
        if is_case_header_line(line) {
            let nested = parse_case_header(line)?
                .ok_or_else(|| "invalid nested `case` header".to_string())?;
            let nested_block = parse_case_block(lines, index, &nested)?;
            index = nested_block.end + 1;
            continue;
        }
        if is_function_header_line(line) {
            let (_, body_end) = function_body_range(lines, index)?;
            index = body_end + 1;
            continue;
        }
        if is_case_clause_header(line) {
            if current_patterns.is_some() {
                return Err("case clause is missing `;;`".to_string());
            }
            let Some(raw) = line.trim().strip_suffix(')') else {
                return Err("case clause header is malformed".to_string());
            };
            let raw = raw.trim();
            let patterns = parse_case_patterns(raw)?;
            if patterns.len() > MAX_CASE_PATTERNS {
                return Err(format!(
                    "case pattern list exceeds the {MAX_CASE_PATTERNS}-pattern limit"
                ));
            }
            current_patterns = Some(patterns);
            current_start = index + 1;
            index += 1;
            continue;
        }
        match line.trim() {
            ";;" => {
                let Some(patterns) = current_patterns.take() else {
                    return Err("case clause terminator has no clause".to_string());
                };
                clauses.push(CaseClause {
                    patterns,
                    body: lines[current_start..index].join("\n"),
                });
            }
            "esac" => {
                if let Some(patterns) = current_patterns.take() {
                    clauses.push(CaseClause {
                        patterns,
                        body: lines[current_start..index].join("\n"),
                    });
                }
                if clauses.is_empty() {
                    return Err("case statement has no clauses".to_string());
                }
                return Ok(CaseBlock {
                    word: header.word.clone(),
                    clauses,
                    end: index,
                });
            }
            "done" | "fi" => {
                return Err("case statement has mismatched control marker".to_string());
            }
            _ => {}
        }
        index += 1;
    }
    Err("case statement is missing `esac`".to_string())
}

fn parse_case_patterns(raw: &str) -> Result<Vec<CasePattern>, String> {
    let alternatives = split_case_pattern_alternatives(raw)?;
    if alternatives.is_empty() {
        return Err("case clause has no pattern".to_string());
    }
    alternatives
        .into_iter()
        .map(|alternative| {
            let plan = parse(&format!("echo {alternative}"))
                .map_err(|error| format!("invalid case pattern: {error}"))?;
            let Some(pipeline) = plan.pipelines.first() else {
                return Err("case pattern is empty".to_string());
            };
            let Some(command) = pipeline.commands.first() else {
                return Err("case pattern has no value".to_string());
            };
            if plan.pipelines.len() != 1
                || !plan.connectors.is_empty()
                || pipeline.commands.len() != 1
                || !command.assignments.is_empty()
                || !command.redirections.is_empty()
                || command.program.literal_value().as_deref() != Some("echo")
                || command.arguments.len() != 1
            {
                return Err(
                    "case patterns support one quoted/literal word with `*`, `?`, or `|`"
                        .to_string(),
                );
            }
            let word = &command.arguments[0];
            let mut tokens = Vec::new();
            for part in word.parts() {
                match part {
                    WordPart::Literal(text) => {
                        tokens.extend(text.chars().map(CasePatternToken::Literal));
                    }
                    WordPart::Wildcard('*') => tokens.push(CasePatternToken::AnySequence),
                    WordPart::Wildcard('?') => tokens.push(CasePatternToken::AnyCharacter),
                    WordPart::Wildcard(other) => {
                        return Err(format!("unsupported case wildcard: {other}"));
                    }
                    WordPart::Variable(_) | WordPart::CommandSubstitution(_) => {
                        return Err(
                            "case patterns do not support variable or command substitution"
                                .to_string(),
                        );
                    }
                }
            }
            Ok(CasePattern { tokens })
        })
        .collect()
}

fn split_case_pattern_alternatives(raw: &str) -> Result<Vec<String>, String> {
    let mut alternatives = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in raw.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') => {
                current.push(character);
                if character == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                current.push(character);
                if character == '"' {
                    quote = None;
                } else if character == '\\' {
                    escaped = true;
                }
            }
            None => match character {
                '\\' => {
                    current.push(character);
                    escaped = true;
                }
                '\'' | '"' => {
                    current.push(character);
                    quote = Some(character);
                }
                '|' => {
                    if current.trim().is_empty() {
                        return Err("case pattern alternative is empty".to_string());
                    }
                    alternatives.push(current.trim().to_string());
                    current.clear();
                }
                _ => current.push(character),
            },
            _ => unreachable!("case pattern quote is one of the supported modes"),
        }
    }
    if escaped || quote.is_some() {
        return Err("case pattern has an unclosed quote or escape".to_string());
    }
    if current.trim().is_empty() {
        return Err("case pattern alternative is empty".to_string());
    }
    alternatives.push(current.trim().to_string());
    Ok(alternatives)
}

fn case_pattern_matches(pattern: &CasePattern, value: &str) -> bool {
    let value = value.chars().collect::<Vec<_>>();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for token in &pattern.tokens {
        let mut current = vec![false; value.len() + 1];
        match token {
            CasePatternToken::AnySequence => {
                current[0] = previous[0];
                for index in 1..=value.len() {
                    current[index] = previous[index] || current[index - 1];
                }
            }
            CasePatternToken::AnyCharacter => {
                current[1..].copy_from_slice(&previous[..value.len()]);
            }
            CasePatternToken::Literal(expected) => {
                for index in 1..=value.len() {
                    current[index] = previous[index - 1] && value[index - 1] == *expected;
                }
            }
        }
        previous = current;
    }
    previous[value.len()]
}

fn parse_if_block(
    lines: &[&str],
    header_index: usize,
    header: &IfHeader,
) -> Result<IfBlock, String> {
    let mut body_start = header_index + 1;
    if !header.inline_then {
        if lines.get(body_start).map(|line| line.trim()) != Some("then") {
            return Err("if statement is missing `then`".to_string());
        }
        body_start += 1;
    }

    let mut blocks = vec![ControlBlock::If];
    let mut clauses = Vec::new();
    let mut current_condition = Some(header.condition.clone());
    let mut current_start = body_start;
    let mut saw_else = false;
    let mut index = body_start;
    while index < lines.len() {
        let line = lines[index];
        if blocks.len() == 1 {
            if is_elif_header_line(line) {
                if saw_else {
                    return Err("if statement has `elif` after `else`".to_string());
                }
                clauses.push(IfClause {
                    condition: current_condition.take(),
                    body: lines[current_start..index].join("\n"),
                });
                let elif = parse_if_header(line, "elif")?
                    .ok_or_else(|| "invalid `elif` header".to_string())?;
                index += 1;
                current_start = index;
                if !elif.inline_then {
                    if lines.get(index).map(|line| line.trim()) != Some("then") {
                        return Err("elif statement is missing `then`".to_string());
                    }
                    index += 1;
                    current_start = index;
                }
                current_condition = Some(elif.condition);
                continue;
            }
            if line.trim() == "else" {
                if saw_else {
                    return Err("if statement has more than one `else`".to_string());
                }
                clauses.push(IfClause {
                    condition: current_condition.take(),
                    body: lines[current_start..index].join("\n"),
                });
                saw_else = true;
                current_start = index + 1;
                index += 1;
                continue;
            }
            if line.trim() == "fi" {
                clauses.push(IfClause {
                    condition: current_condition.take(),
                    body: lines[current_start..index].join("\n"),
                });
                return Ok(IfBlock {
                    clauses,
                    end: index,
                });
            }
        }

        if is_for_header_line(line) || is_loop_header_line(line) {
            blocks.push(ControlBlock::Loop);
        } else if is_if_header_line(line) {
            blocks.push(ControlBlock::If);
        } else if is_case_header_line(line) {
            blocks.push(ControlBlock::Case);
        } else if is_function_header_line(line) {
            blocks.push(ControlBlock::Function);
        } else {
            match line.trim() {
                "done" => match blocks.pop() {
                    Some(ControlBlock::Loop) => {}
                    _ => return Err("if statement has mismatched `done`/`fi`".to_string()),
                },
                "fi" if blocks.pop() != Some(ControlBlock::If) => {
                    return Err("if statement has mismatched `done`/`fi`".to_string());
                }
                "esac" if blocks.pop() != Some(ControlBlock::Case) => {
                    return Err("if statement has mismatched `esac`".to_string());
                }
                "}" if blocks.pop() != Some(ControlBlock::Function) => {
                    return Err("if statement has mismatched `}`".to_string());
                }
                _ => {}
            }
        }
        index += 1;
    }
    Err("if statement is missing `fi`".to_string())
}

struct ExpandedWord {
    value: String,
    has_wildcard: bool,
    stderr: String,
}

fn history_entry(line: &str, redact: bool) -> String {
    if !redact {
        return line.to_string();
    }
    let Ok(plan) = parse(line) else {
        return line.to_string();
    };
    let contains_environment_setter = plan_contains_environment_setter(&plan, 0);
    let contains_network_request = plan_contains_network_request(&plan, 0);
    let contains_private_action = plan_contains_private_action(&plan, 0);
    if contains_network_request {
        return "[redacted network command]".to_string();
    }
    if contains_private_action {
        return "[redacted private command]".to_string();
    }
    if contains_environment_setter {
        "[redacted environment assignment]".to_string()
    } else {
        line.to_string()
    }
}

fn plan_contains_environment_setter(plan: &ExecutionPlan, depth: usize) -> bool {
    plan.pipelines.iter().any(|pipeline| {
        pipeline.commands.iter().any(|command| {
            !command.assignments.is_empty()
                || matches!(
                    command.program.literal_value().as_deref(),
                    Some("export" | "setenv")
                )
                || command_words_contain(command, plan_contains_environment_setter, depth)
        })
    })
}

fn plan_contains_network_request(plan: &ExecutionPlan, depth: usize) -> bool {
    plan.pipelines.iter().any(|pipeline| {
        pipeline.commands.iter().any(|command| {
            if matches!(
                command.program.literal_value().as_deref(),
                Some("curl" | "nslookup" | "whois")
            ) || command_builtin_contains_network_request(command)
            {
                return true;
            }
            (command.program.literal_value().as_deref() == Some("pkg")
                && command.arguments.iter().any(|argument| {
                    matches!(
                        argument.literal_value().as_deref(),
                        Some("--registry" | "--remote")
                    )
                }))
                || command_words_contain(command, plan_contains_network_request, depth)
        })
    })
}

fn plan_contains_private_action(plan: &ExecutionPlan, depth: usize) -> bool {
    plan.pipelines.iter().any(|pipeline| {
        pipeline.commands.iter().any(|command| {
            matches!(
                command.program.literal_value().as_deref(),
                Some("call" | "text")
            ) || command_words_contain(command, plan_contains_private_action, depth)
        })
    })
}

fn command_builtin_contains_network_request(command: &rune_shell::CommandPlan) -> bool {
    if command.program.literal_value().as_deref() != Some("command") {
        return false;
    }
    let mut arguments = command.arguments.iter();
    let Some(first) = arguments.next().and_then(Word::literal_value) else {
        return false;
    };
    if matches!(first.as_str(), "-v" | "-V") {
        return false;
    }
    let target = if first == "--" {
        arguments.next().and_then(Word::literal_value)
    } else {
        Some(first)
    };
    match target.as_deref() {
        Some("curl" | "nslookup" | "whois") => true,
        Some("pkg") => arguments.any(|argument| {
            matches!(
                argument.literal_value().as_deref(),
                Some("--registry" | "--remote")
            )
        }),
        _ => false,
    }
}

fn command_words_contain(
    command: &rune_shell::CommandPlan,
    predicate: fn(&ExecutionPlan, usize) -> bool,
    depth: usize,
) -> bool {
    if depth >= MAX_COMMAND_SUBSTITUTION_DEPTH {
        return true;
    }
    command
        .assignments
        .iter()
        .any(|assignment| word_contains(&assignment.value, predicate, depth))
        || word_contains(&command.program, predicate, depth)
        || command
            .arguments
            .iter()
            .any(|argument| word_contains(argument, predicate, depth))
        || command
            .redirections
            .iter()
            .any(|redirection| match redirection {
                Redirection::Stdin { path }
                | Redirection::Stdout { path, .. }
                | Redirection::Stderr { path, .. }
                | Redirection::Both { path, .. } => word_contains(path, predicate, depth),
                Redirection::StdoutToStderr | Redirection::StderrToStdout => false,
            })
}

fn word_contains(word: &Word, predicate: fn(&ExecutionPlan, usize) -> bool, depth: usize) -> bool {
    word.parts().iter().any(|part| {
        let WordPart::CommandSubstitution(command) = part else {
            return false;
        };
        let Ok(plan) = parse(command) else {
            return true;
        };
        predicate(&plan, depth + 1)
    })
}

fn emit_output_chunks(sink: &mut dyn EventSink, stdout: &str, stderr: &str) {
    let stdout_chunks = output_chunks(stdout);
    let stderr_chunks = output_chunks(stderr);
    let chunk_count = stdout_chunks.len().max(stderr_chunks.len()).max(1);
    for index in 0..chunk_count {
        sink.emit(CommandEvent::Output {
            stdout: stdout_chunks
                .get(index)
                .copied()
                .unwrap_or_default()
                .to_owned(),
            stderr: stderr_chunks
                .get(index)
                .copied()
                .unwrap_or_default()
                .to_owned(),
        });
    }
}

fn output_chunks(value: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < value.len() {
        let mut end = (start + MAX_EVENT_CHUNK_BYTES).min(value.len());
        while end > start && !value.is_char_boundary(end) {
            end -= 1;
        }
        // MAX_EVENT_CHUNK_BYTES is larger than the maximum UTF-8 scalar, so
        // this is defensive rather than an expected path.
        if end == start {
            end = value
                .char_indices()
                .find_map(|(offset, character)| {
                    (offset > start).then_some(offset + character.len_utf8())
                })
                .unwrap_or(value.len());
        }
        chunks.push(&value[start..end]);
        start = end;
    }
    chunks
}

fn limit_output(output: &mut CommandOutput) {
    let stdout_truncated = truncate_channel(&mut output.stdout);
    let stderr_truncated = truncate_channel(&mut output.stderr);
    if stdout_truncated && !stderr_truncated {
        output.stderr.push_str(OUTPUT_TRUNCATION_MARKER);
    }
}

fn truncate_channel(channel: &mut String) -> bool {
    if channel.len() <= MAX_OUTPUT_BYTES {
        return false;
    }
    let retained = MAX_OUTPUT_BYTES.saturating_sub(OUTPUT_TRUNCATION_MARKER.len());
    let mut end = retained;
    while !channel.is_char_boundary(end) {
        end -= 1;
    }
    channel.truncate(end);
    channel.push_str(OUTPUT_TRUNCATION_MARKER);
    true
}

pub(crate) fn usage(command: &str, message: &str) -> CommandOutput {
    CommandOutput::failure(2, format!("{command}: {message}\n"))
}

pub(crate) fn fs_failure(command: &str, error: &FsError) -> CommandOutput {
    CommandOutput::failure(1, format!("{command}: {error}\n"))
}

fn package_runtime_failure(command: &str, error: &rune_package::PackageError) -> CommandOutput {
    CommandOutput::failure(
        126,
        format!("{command}: installed package integrity failure: {error}\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        persistence::MAX_HISTORY_BYTES, ClipboardError, ClipboardProvider, CommandEvent,
        CommandOutput, EventSink, NetworkError, NetworkMethod, NetworkProvider, NetworkRequest,
        NetworkResponse, OpenError, OpenProvider, OpenRequest, OpenTargetKind, Session,
        SessionAction, TerminalConfig, ToolchainArtifact, ToolchainError, ToolchainKind,
        ToolchainOutput, ToolchainProvider, ToolchainRequest, CANCELLED_STATUS, MAX_BOOKMARKS,
        MAX_BOOKMARK_NAME_CHARS, MAX_CLIPBOARD_BYTES, MAX_COMMAND_INPUT_BYTES,
        MAX_EVENT_CHUNK_BYTES, MAX_FILE_TRANSFER_BYTES, MAX_FOR_VALUES, MAX_FUNCTIONS,
        MAX_FUNCTION_ARGUMENTS, MAX_FUNCTION_DEPTH, MAX_FUNCTION_NAME_BYTES, MAX_LOCAL_VARIABLES,
        MAX_OUTPUT_BYTES, MAX_SCRIPT_BYTES, MAX_SCRIPT_CONTROL_DEPTH, MAX_SCRIPT_LINES,
        MAX_SOURCE_ARGUMENTS, MAX_SOURCE_DEPTH, MAX_WHILE_ITERATIONS, OUTPUT_TRUNCATION_MARKER,
    };
    use rune_fs::SandboxedFileSystem;
    use std::fmt::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root() -> std::path::PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        loop {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos();
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "rune-core-test-{}-{timestamp}-{id}",
                std::process::id()
            ));
            match std::fs::create_dir(&root) {
                Ok(()) => return root,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("test root could not be created: {error}"),
            }
        }
    }

    #[derive(Default)]
    struct RecordingEventSink {
        events: Vec<CommandEvent>,
    }

    impl EventSink for RecordingEventSink {
        fn emit(&mut self, event: CommandEvent) {
            self.events.push(event);
        }
    }

    struct RecordingNetworkProvider {
        requests: Arc<Mutex<Vec<NetworkRequest>>>,
        response: NetworkResponse,
    }

    impl NetworkProvider for RecordingNetworkProvider {
        fn request(&self, request: &NetworkRequest) -> Result<NetworkResponse, NetworkError> {
            self.requests
                .lock()
                .expect("request log lock")
                .push(request.clone());
            Ok(self.response.clone())
        }
    }

    struct RoutingNetworkProvider {
        requests: Arc<Mutex<Vec<NetworkRequest>>>,
        routes: Vec<(String, NetworkResponse)>,
    }

    impl NetworkProvider for RoutingNetworkProvider {
        fn request(&self, request: &NetworkRequest) -> Result<NetworkResponse, NetworkError> {
            self.requests
                .lock()
                .expect("request log lock")
                .push(request.clone());
            self.routes
                .iter()
                .find(|(url, _)| url == &request.url)
                .map(|(_, response)| response.clone())
                .ok_or_else(|| NetworkError::Transport("test route not found".to_string()))
        }
    }

    struct RecordingClipboardProvider {
        value: Arc<Mutex<String>>,
    }

    impl ClipboardProvider for RecordingClipboardProvider {
        fn read_text(&self) -> Result<String, ClipboardError> {
            Ok(self.value.lock().expect("clipboard lock").clone())
        }

        fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
            *self.value.lock().expect("clipboard lock") = text.to_string();
            Ok(())
        }
    }

    struct RecordingOpenProvider {
        requests: Arc<Mutex<Vec<OpenRequest>>>,
    }

    impl OpenProvider for RecordingOpenProvider {
        fn open(&self, request: &OpenRequest) -> Result<(), OpenError> {
            self.requests
                .lock()
                .expect("open request log lock")
                .push(request.clone());
            Ok(())
        }
    }

    struct RecordingToolchainProvider {
        kind: ToolchainKind,
    }

    impl ToolchainProvider for RecordingToolchainProvider {
        fn kind(&self) -> ToolchainKind {
            self.kind
        }

        fn execute(
            &self,
            request: &ToolchainRequest<'_>,
        ) -> Result<ToolchainOutput, ToolchainError> {
            request.validate()?;
            Ok(ToolchainOutput {
                stdout: format!("compiled {}\n", request.program_name),
                stderr: String::new(),
                status: 0,
                artifacts: vec![ToolchainArtifact {
                    path: "hello.wasm".to_string(),
                    media_type: "application/wasm".to_string(),
                    bytes: b"fake-compiled-artifact".to_vec(),
                }],
            })
        }
    }

    #[test]
    fn emits_pipeline_output_and_line_status_events() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let mut sink = RecordingEventSink::default();

        let output = session.execute_line_with_events("echo first; false; echo last", &mut sink);
        assert_eq!(output.stdout, "first\nlast\n");
        assert_eq!(output.status, 0);
        assert_eq!(
            sink.events,
            vec![
                CommandEvent::Output {
                    stdout: "first\n".to_string(),
                    stderr: String::new(),
                },
                CommandEvent::Output {
                    stdout: String::new(),
                    stderr: String::new(),
                },
                CommandEvent::Output {
                    stdout: "last\n".to_string(),
                    stderr: String::new(),
                },
                CommandEvent::Status {
                    status: 0,
                    current_directory: "~".to_string(),
                },
            ]
        );
        assert_eq!(session.terminal_snapshot(), "first\nlast");

        sink.events.clear();
        let parse_error = session.execute_line_with_events("echo 'unfinished", &mut sink);
        assert_eq!(parse_error.status, 2);
        assert!(matches!(
            sink.events.as_slice(),
            [
                CommandEvent::Output { stderr, .. },
                CommandEvent::Status { status: 2, .. }
            ] if stderr.contains("unclosed single quote")
        ));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn chunks_large_pipeline_events_without_splitting_utf8() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let expected = "🙂".repeat((MAX_EVENT_CHUNK_BYTES / "🙂".len()) * 3 + 7);
        std::fs::write(root.join("large.txt"), expected.as_bytes()).expect("large file written");
        let mut sink = RecordingEventSink::default();

        let output = session.execute_line_with_events("cat large.txt", &mut sink);
        assert_eq!(output.status, 0);
        let mut emitted = String::new();
        let mut output_events = 0;
        for event in &sink.events {
            if let CommandEvent::Output { stdout, stderr } = event {
                assert!(stdout.len() <= MAX_EVENT_CHUNK_BYTES);
                assert!(stderr.len() <= MAX_EVENT_CHUNK_BYTES);
                assert!(stdout.is_char_boundary(stdout.len()));
                assert!(stderr.is_char_boundary(stderr.len()));
                if !stdout.is_empty() {
                    assert_eq!(stdout.len() % "🙂".len(), 0);
                }
                emitted.push_str(stdout);
                output_events += 1;
            }
        }
        assert!(output_events > 1);
        assert_eq!(emitted, output.stdout);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn keeps_a_rust_owned_terminal_snapshot_in_sync_with_command_output() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let progress = session.execute_line("printf 'progress\r\u{1b}[2Kready'");
        assert_eq!(progress.status, 0);
        assert_eq!(session.terminal_snapshot(), "ready");
        assert_eq!(session.execute_line("printf '\nnext'").status, 0);
        assert_eq!(session.terminal_snapshot(), "ready\nnext");
        assert_eq!(session.execute_line("clear").status, 0);
        assert!(session.terminal_snapshot().is_empty());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn feeds_the_rust_terminal_snapshot_once_per_streamed_script_event() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let output = session.execute_script("printf 'first\\n'\nprintf 'second'");
        assert_eq!(output.status, 0);
        assert_eq!(session.terminal_snapshot(), "first\nsecond");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn emits_events_for_each_script_line() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let mut sink = RecordingEventSink::default();

        let output = session.execute_script_with_events("echo first\nfalse\necho last", &mut sink);
        assert_eq!(output.stdout, "first\nlast\n");
        assert_eq!(output.status, 0);
        assert_eq!(
            sink.events
                .iter()
                .filter(|event| matches!(event, CommandEvent::Status { .. }))
                .count(),
            4
        );
        assert_eq!(
            sink.events
                .iter()
                .filter(|event| matches!(event, CommandEvent::Output { .. }))
                .count(),
            3
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn sourced_script_emits_aggregate_output_once() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        std::fs::write(root.join("script.rune"), "printf script-ok").expect("script written");
        let mut sink = RecordingEventSink::default();

        let output = session.execute_line_with_events("source script.rune", &mut sink);
        assert_eq!(output.stdout, "script-ok");
        assert_eq!(
            sink.events
                .iter()
                .filter_map(|event| match event {
                    CommandEvent::Output { stdout, .. } => Some(stdout.as_str()),
                    CommandEvent::Status { .. } => None,
                })
                .collect::<String>(),
            output.stdout
        );
        assert_eq!(
            sink.events
                .iter()
                .filter(|event| matches!(event, CommandEvent::Output { .. }))
                .count(),
            1
        );
        std::fs::remove_dir_all(root).expect("root removed");
    }

    #[test]
    fn nested_shell_and_function_emit_aggregate_output_once() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let definition = session.execute_script("emit() {\nprintf fn-ok\n}");
        assert_eq!(definition.status, 0);

        for (command, expected) in [("sh -c 'printf sh-ok'", "sh-ok"), ("emit", "fn-ok")] {
            let mut sink = RecordingEventSink::default();
            let output = session.execute_line_with_events(command, &mut sink);
            assert_eq!(output.stdout, expected);
            assert_eq!(
                sink.events
                    .iter()
                    .filter_map(|event| match event {
                        CommandEvent::Output { stdout, .. } => Some(stdout.as_str()),
                        CommandEvent::Status { .. } => None,
                    })
                    .collect::<String>(),
                expected
            );
            assert_eq!(
                sink.events
                    .iter()
                    .filter(|event| matches!(event, CommandEvent::Output { .. }))
                    .count(),
                1
            );
        }
        std::fs::remove_dir_all(root).expect("root removed");
    }

    #[test]
    fn executes_real_filesystem_workflow() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("pwd").stdout, "~\n");
        assert_eq!(session.execute_line("mkdir work").status, 0);
        assert_eq!(session.execute_line("cd work").status, 0);
        assert_eq!(session.execute_line("echo hello > note.txt").status, 0);
        assert_eq!(session.execute_line("cat note.txt").stdout, "hello\n");
        assert_eq!(session.execute_line("ln -s note.txt note-link").status, 0);
        assert_eq!(
            session.execute_line("readlink note-link").stdout,
            "note.txt\n"
        );
        assert_eq!(
            session.execute_line("realpath note-link").stdout,
            "~/work/note.txt\n"
        );
        assert_eq!(
            session.execute_line("sha256 note.txt").stdout,
            format!("{}  note.txt\n", rune_package::sha256_hex(b"hello\n"))
        );
        assert_eq!(
            session.execute_line("printf hello | sha256").stdout,
            format!("{}  -\n", rune_package::sha256_hex(b"hello"))
        );
        assert_eq!(session.execute_line("cat note-link").stdout, "hello\n");
        assert!(session
            .execute_line("ln -s ../missing.txt dangling-link")
            .stderr
            .contains("no such file or directory"));
        let missing = session.execute_line("realpath missing.txt");
        assert_eq!(missing.status, 1);
        assert!(missing.stderr.contains("no such file or directory"));
        assert_eq!(session.execute_line("pwd").stdout, "~/work\n");
        assert_eq!(session.execute_line("mkdir -p source/nested").status, 0);
        assert_eq!(
            session
                .execute_line("echo copied > source/nested/value.txt")
                .status,
            0
        );
        assert_eq!(session.execute_line("cp -r source copy").status, 0);
        assert_eq!(
            session.execute_line("cat copy/nested/value.txt").stdout,
            "copied\n"
        );
        assert_eq!(session.execute_line("mv copy moved").status, 0);
        assert_eq!(
            session.execute_line("cat moved/nested/value.txt").stdout,
            "copied\n"
        );
        session.persist().expect("state persisted");
        let restored = Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert_eq!(restored.current_directory(), "~/work");
        assert!(restored.history().contains(&"cat note.txt".to_string()));
        assert_eq!(
            restored.history().last().map(String::as_str),
            Some("cat moved/nested/value.txt")
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn persists_and_restores_the_bounded_terminal_screen() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let output = session.execute_line("printf 'stale\r\u{1b}[2Kready'");
        assert_eq!(output.status, 0);
        assert_eq!(session.terminal_snapshot(), "ready");
        assert_eq!(session.terminal_cursor_position(), (0, 5));
        session.persist().expect("terminal state persisted");

        let restored = Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert_eq!(restored.terminal_snapshot(), "ready");
        assert_eq!(restored.terminal_cursor_position(), (0, 5));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn applies_ordered_stream_duplication_and_append_redirections() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let merged = session.execute_line("cat missing.txt 2>&1");
        assert_eq!(merged.status, 1);
        assert!(merged.stdout.contains("no such file or directory"));
        assert!(merged.stderr.is_empty());

        let ordered = session.execute_line("cat missing.txt 2>&1 > only-stdout.txt");
        assert_eq!(ordered.status, 1);
        assert!(ordered.stdout.contains("no such file or directory"));
        assert!(ordered.stderr.is_empty());
        assert!(session
            .execute_line("cat only-stdout.txt")
            .stdout
            .is_empty());

        let both = session.execute_line("cat missing.txt &> all-output.txt");
        assert_eq!(both.status, 1);
        assert!(both.stdout.is_empty());
        assert!(both.stderr.is_empty());
        let all_output = session.execute_line("cat all-output.txt");
        assert_eq!(all_output.status, 0);
        assert!(all_output.stdout.contains("no such file or directory"));

        assert_eq!(session.execute_line("echo first > append.txt").status, 0);
        assert_eq!(session.execute_line("echo second >> append.txt").status, 0);
        assert_eq!(
            session.execute_line("cat append.txt").stdout,
            "first\nsecond\n"
        );

        let replaced = session.execute_line("echo retained > created-then-replaced.txt 1>&2");
        assert_eq!(replaced.status, 0);
        assert!(replaced.stdout.is_empty());
        assert_eq!(replaced.stderr, "retained\n");
        assert!(session
            .execute_line("cat created-then-replaced.txt")
            .stdout
            .is_empty());

        let to_stderr = session.execute_line("echo redirected 1>&2");
        assert_eq!(to_stderr.status, 0);
        assert!(to_stderr.stdout.is_empty());
        assert_eq!(to_stderr.stderr, "redirected\n");

        let piped = session.execute_line("cat missing-again.txt 2>&1 | cat");
        assert_eq!(piped.status, 0);
        assert!(piped.stdout.contains("no such file or directory"));
        assert!(piped.stderr.is_empty());

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn routes_curl_through_an_explicit_bounded_network_provider() {
        let root = test_root();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_network_provider(Box::new(RecordingNetworkProvider {
            requests: Arc::clone(&requests),
            response: NetworkResponse {
                status_code: 200,
                body: b"response body\n".to_vec(),
            },
        }));

        let fetched = session.execute_line("curl https://example.test/data");
        assert_eq!(fetched.status, 0);
        assert_eq!(fetched.stdout, "response body\n");
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted network command]")
        );

        let saved = session.execute_line(
            "curl -X POST -H 'X-Test: yes' -d payload -o response.txt https://example.test/upload",
        );
        assert_eq!(saved.status, 0);
        assert_eq!(
            session.execute_line("cat response.txt").stdout,
            "response body\n"
        );
        let recorded = requests.lock().expect("request log lock");
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].method, NetworkMethod::Get);
        assert_eq!(recorded[0].url, "https://example.test/data");
        assert_eq!(recorded[1].method, NetworkMethod::Post);
        assert_eq!(recorded[1].body, b"payload");
        assert_eq!(
            recorded[1].headers,
            [("X-Test".to_string(), "yes".to_string())]
        );
        drop(recorded);

        let bypassed = session.execute_line("command curl https://example.test/bypass");
        assert_eq!(bypassed.status, 0);
        assert_eq!(bypassed.stdout, "response body\n");
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted network command]")
        );

        let disabled = Session::new(SandboxedFileSystem::new(&root).expect("root reopened"))
            .execute_line("curl https://example.test");
        assert_eq!(disabled.status, 1);
        assert!(disabled.stderr.contains("network provider is unavailable"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn routes_nslookup_through_doh_and_redacts_the_query_from_history() {
        let root = test_root();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = "https://resolver.example.test/dns-query";
        let query_url = format!("{server}?name=example.com&type=AAAA");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_network_provider(Box::new(RoutingNetworkProvider {
            requests: Arc::clone(&requests),
            routes: vec![(
                query_url,
                NetworkResponse {
                    status_code: 200,
                    body: br#"{
                            "Status": 0,
                            "Answer": [
                                {"name":"example.com.","type":28,"TTL":60,"data":"2001:db8::1"},
                                {"name":"example.com.","type":28,"TTL":60,"data":"2001:db8::2"}
                            ]
                        }"#
                    .to_vec(),
                },
            )],
        }));

        let resolved = session.execute_line(&format!(
            "nslookup --server {server} -type=AAAA example.com"
        ));
        assert_eq!(resolved.status, 0, "{resolved:?}");
        assert_eq!(resolved.stdout, "2001:db8::1\n2001:db8::2\n");
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted network command]")
        );
        let host_resolved =
            session.execute_line(&format!("host --server {server} -type=AAAA example.com"));
        assert_eq!(host_resolved.status, 0, "{host_resolved:?}");
        assert_eq!(host_resolved.stdout, "2001:db8::1\n2001:db8::2\n");

        let recorded = requests.lock().expect("request log lock");
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].method, NetworkMethod::Get);
        assert_eq!(
            recorded[0].url,
            format!("{server}?name=example.com&type=AAAA")
        );
        assert_eq!(
            recorded[0].headers,
            [("Accept".to_string(), "application/dns-json".to_string())]
        );
        assert!(recorded[0].body.is_empty());
        assert_eq!(recorded[1], recorded[0]);
        drop(recorded);

        let invalid_type = session.execute_line("nslookup -type=HTTPS example.com");
        assert_eq!(invalid_type.status, 2);
        assert!(invalid_type
            .stderr
            .contains("unsupported record type HTTPS"));
        let invalid_server =
            session.execute_line("nslookup --server http://resolver.test example.com");
        assert_eq!(invalid_server.status, 2);
        assert!(invalid_server.stderr.contains("must use an https://"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn reports_dns_status_and_empty_answers_without_exposing_response_json() {
        let root = test_root();
        let server = "https://resolver.example.test/dns-query";
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_network_provider(Box::new(RoutingNetworkProvider {
            requests: Arc::new(Mutex::new(Vec::new())),
            routes: vec![
                (
                    format!("{server}?name=missing.example&type=A"),
                    NetworkResponse {
                        status_code: 200,
                        body: br#"{"Status":3,"Comment":"private resolver detail"}"#.to_vec(),
                    },
                ),
                (
                    format!("{server}?name=empty.example&type=A"),
                    NetworkResponse {
                        status_code: 200,
                        body: br#"{"Status":0,"Answer":[]}"#.to_vec(),
                    },
                ),
            ],
        }));

        let dns_error =
            session.execute_line(&format!("nslookup --server {server} missing.example"));
        assert_eq!(dns_error.status, 1);
        assert_eq!(
            dns_error.stderr,
            "nslookup: resolver returned DNS status 3 for missing.example\n"
        );
        assert!(!dns_error.stderr.contains("private resolver detail"));

        let empty = session.execute_line(&format!("nslookup --server {server} empty.example"));
        assert_eq!(empty.status, 1);
        assert_eq!(
            empty.stderr,
            "nslookup: no A records found for empty.example\n"
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn routes_whois_through_https_rdap_with_bounded_text_output() {
        let root = test_root();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server = "https://rdap.example.test/domain";
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_network_provider(Box::new(RoutingNetworkProvider {
            requests: Arc::clone(&requests),
            routes: vec![(
                format!("{server}/example.com"),
                NetworkResponse {
                    status_code: 200,
                    body: b"domain: example.com\nstatus: active\n".to_vec(),
                },
            )],
        }));

        let record = session.execute_line(&format!("whois --server {server} example.com"));
        assert_eq!(record.status, 0, "{record:?}");
        assert_eq!(record.stdout, "domain: example.com\nstatus: active\n");
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted network command]")
        );

        let recorded = requests.lock().expect("request log lock");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].method, NetworkMethod::Get);
        assert_eq!(recorded[0].url, format!("{server}/example.com"));
        assert_eq!(
            recorded[0].headers,
            [(
                "Accept".to_string(),
                "application/rdap+json, application/json, text/plain".to_string()
            )]
        );
        assert!(recorded[0].body.is_empty());
        drop(recorded);

        let invalid_target = session.execute_line("whois example.com/secret");
        assert_eq!(invalid_target.status, 2);
        assert!(invalid_target.stderr.contains("ASCII DNS label characters"));
        let invalid_server = session.execute_line("whois --server http://rdap.test example.com");
        assert_eq!(invalid_server.status, 2);
        assert!(invalid_server.stderr.contains("must use an https://"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn routes_open_through_an_explicit_host_provider() {
        let root = test_root();
        std::fs::write(root.join("note.txt"), b"open me\n").expect("file written");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_open_provider(Box::new(RecordingOpenProvider {
            requests: Arc::clone(&requests),
        }));

        let opened_file = session.execute_line("open note.txt");
        assert_eq!(opened_file.status, 0);
        let opened_url = session.execute_line("openurl https://example.test/docs");
        assert_eq!(opened_url.status, 0);
        let played_file = session.execute_line("play note.txt");
        assert_eq!(played_file.status, 0);
        let viewed_file = session.execute_line("view note.txt");
        assert_eq!(viewed_file.status, 0);

        let recorded = requests.lock().expect("open request log lock");
        assert_eq!(recorded.len(), 4);
        assert_eq!(recorded[0].kind, OpenTargetKind::File);
        let expected_file_path =
            std::fs::canonicalize(root.join("note.txt")).expect("file path canonicalized");
        assert_eq!(
            recorded[0].target,
            expected_file_path.to_string_lossy().as_ref()
        );
        assert_eq!(recorded[1].kind, OpenTargetKind::Url);
        assert_eq!(recorded[1].target, "https://example.test/docs");
        assert_eq!(recorded[2].kind, OpenTargetKind::Play);
        assert_eq!(
            recorded[2].target,
            expected_file_path.to_string_lossy().as_ref()
        );
        assert_eq!(recorded[3].kind, OpenTargetKind::View);
        assert_eq!(
            recorded[3].target,
            expected_file_path.to_string_lossy().as_ref()
        );
        drop(recorded);

        let invalid_scheme = session.execute_line("openurl ftp://example.test/file");
        assert_eq!(invalid_scheme.status, 2);
        let escaped = session.execute_line("open ../outside.txt");
        assert_eq!(escaped.status, 1);
        assert!(escaped.stderr.contains("sandbox"));
        assert_eq!(requests.lock().expect("open request log lock").len(), 4);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn routes_direct_phone_actions_through_the_host_provider() {
        let root = test_root();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_open_provider(Box::new(RecordingOpenProvider {
            requests: Arc::clone(&requests),
        }));

        let called = session.execute_line(r#"call "+33 (6) 12-34-56-78""#);
        assert_eq!(called.status, 0, "{called:?}");
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted private command]")
        );
        let texting = session.execute_line(r#"text +33612345678 "hello world&é""#);
        assert_eq!(texting.status, 0, "{texting:?}");

        let recorded = requests.lock().expect("open request log lock");
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].kind, OpenTargetKind::Url);
        assert_eq!(recorded[0].target, "tel://+33612345678");
        assert_eq!(recorded[1].kind, OpenTargetKind::Url);
        assert_eq!(
            recorded[1].target,
            "sms://+33612345678&body=hello%20world%26%C3%A9"
        );
        drop(recorded);

        let invalid_characters = session.execute_line("call +336123ABC");
        assert_eq!(invalid_characters.status, 2);
        let too_short = session.execute_line("call 12");
        assert_eq!(too_short.status, 2);
        let missing_number = session.execute_line("text");
        assert_eq!(missing_number.status, 2);
        assert_eq!(requests.lock().expect("open request log lock").len(), 2);

        let disabled = Session::new(SandboxedFileSystem::new(&root).expect("root reopened"))
            .execute_line("call 123");
        assert_eq!(disabled.status, 1);
        assert!(disabled.stderr.contains("provider is unavailable"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn evaluates_bounded_test_and_bracket_predicates_inside_shell_conditionals() {
        let root = test_root();
        std::fs::write(root.join("note.txt"), b"hello\n").expect("file written");
        std::fs::write(root.join("empty.txt"), b"").expect("empty file written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        assert_eq!(session.execute_line("test -f note.txt").status, 0);
        assert_eq!(session.execute_line("[ -d . ]").status, 0);
        assert_eq!(session.execute_line("test -s empty.txt").status, 1);
        assert_eq!(session.execute_line("test -e missing.txt").status, 1);
        assert_eq!(session.execute_line("test -n value -a 7 -ge 3").status, 0);
        assert_eq!(session.execute_line("test 7 -lt 3").status, 1);
        assert_eq!(session.execute_line("[ left = left ]").status, 0);
        assert_eq!(session.execute_line("test ! -z value").status, 0);

        let conditional = session.execute_line("test -f note.txt && echo readable");
        assert_eq!(conditional.status, 0);
        assert_eq!(conditional.stdout, "readable\n");

        assert_eq!(session.execute_line("ln -s note.txt note-link").status, 0);
        assert_eq!(session.execute_line("test -L note-link").status, 0);
        assert_eq!(session.execute_line("[ -f note.txt").status, 2);
        assert_eq!(session.execute_line("test -e ../outside").status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn rejects_open_targets_before_the_host_provider() {
        let root = test_root();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_open_provider(Box::new(RecordingOpenProvider {
            requests: Arc::clone(&requests),
        }));

        assert_eq!(session.execute_line("mkdir folder").status, 0);
        assert_eq!(session.execute_line("ln -s folder folder-link").status, 0);
        let invalid_url = session.execute_line("openurl https://");
        assert_eq!(invalid_url.status, 2);
        assert!(invalid_url.stderr.contains("host"));
        let invalid_scheme = session.execute_line("openurl javascript:alert");
        assert_eq!(invalid_scheme.status, 2);
        assert!(invalid_scheme.stderr.contains("not allowed"));
        let missing_media = session.execute_line("play missing.mp4");
        assert_eq!(missing_media.status, 1);
        assert!(missing_media.stderr.contains("no such file"));
        let directory_preview = session.execute_line("view .");
        assert_eq!(directory_preview.status, 1);
        assert!(directory_preview.stderr.contains("directory"));
        let symlink_directory_preview = session.execute_line("view folder-link");
        assert_eq!(symlink_directory_preview.status, 1);
        assert!(symlink_directory_preview.stderr.contains("directory"));
        assert!(requests.lock().expect("open request log lock").is_empty());

        let disabled = Session::new(SandboxedFileSystem::new(&root).expect("root reopened"))
            .execute_line("openurl https://example.test");
        assert_eq!(disabled.status, 1);
        assert!(disabled.stderr.contains("provider is unavailable"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[allow(clippy::too_many_lines)]
    #[test]
    fn searches_installs_and_updates_a_verified_remote_package() {
        let root = test_root();
        let index_url = "https://registry.example.test/index.json";
        let manifest_v1_url = "https://registry.example.test/remote/v1/manifest.json";
        let artifact_v1_url = "https://registry.example.test/remote/v1/bin/remote.rune";
        let manifest_v2_url = "https://registry.example.test/remote/v2/manifest.json";
        let artifact_v2_url = "https://registry.example.test/remote/v2/bin/remote.rune";
        let script_v1 = b"echo remote-v1\n";
        let script_v2 = b"echo remote-v2\n";
        let manifest_v1 = format!(
            r#"{{
                "schema_version": 1,
                "name": "remote-tool",
                "version": "0.1.0",
                "description": "A remote Rune tool",
                "files": [{{"path": "bin/remote.rune", "sha256": "{}"}}],
                "commands": [{{"name": "remote-tool", "entry": "bin/remote.rune"}}]
            }}"#,
            rune_package::sha256_hex(script_v1)
        );
        let manifest_v2 = format!(
            r#"{{
                "schema_version": 1,
                "name": "remote-tool",
                "version": "0.2.0",
                "description": "A remote Rune tool",
                "files": [{{"path": "bin/remote.rune", "sha256": "{}"}}],
                "commands": [{{"name": "remote-tool", "entry": "bin/remote.rune"}}]
            }}"#,
            rune_package::sha256_hex(script_v2)
        );
        let index = format!(
            r#"{{
                "schema_version": 1,
                "packages": [
                    {{
                        "name": "remote-tool",
                        "version": "0.1.0",
                        "description": "A remote Rune tool",
                        "manifest_url": "{manifest_v1_url}",
                        "artifacts": [{{"path": "bin/remote.rune", "url": "{artifact_v1_url}"}}]
                    }},
                    {{
                        "name": "remote-tool",
                        "version": "0.2.0",
                        "description": "A remote Rune tool",
                        "manifest_url": "{manifest_v2_url}",
                        "artifacts": [{{"path": "bin/remote.rune", "url": "{artifact_v2_url}"}}]
                    }}
                ]
            }}"#
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_network_provider(Box::new(RoutingNetworkProvider {
            requests: Arc::clone(&requests),
            routes: vec![
                (
                    index_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: index.into_bytes(),
                    },
                ),
                (
                    manifest_v1_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: manifest_v1.into_bytes(),
                    },
                ),
                (
                    artifact_v1_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: script_v1.to_vec(),
                    },
                ),
                (
                    manifest_v2_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: manifest_v2.into_bytes(),
                    },
                ),
                (
                    artifact_v2_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: script_v2.to_vec(),
                    },
                ),
            ],
        }));

        let search = session.execute_line(&format!("pkg search --registry {index_url} remote"));
        assert_eq!(search.status, 0, "{search:?}");
        assert_eq!(
            search.stdout,
            "remote-tool@0.1.0\tA remote Rune tool\nremote-tool@0.2.0\tA remote Rune tool\n"
        );
        let installed = session.execute_line(&format!(
            "pkg install --registry {index_url} remote-tool 0.1.0"
        ));
        assert_eq!(installed.status, 0, "{installed:?}");
        assert_eq!(
            installed.stdout,
            "installed remote-tool@0.1.0 from registry\n"
        );
        assert_eq!(session.execute_line("remote-tool").stdout, "remote-v1\n");

        let updated = session.execute_line(&format!(
            "pkg update --registry {index_url} remote-tool 0.2.0"
        ));
        assert_eq!(updated.status, 0, "{updated:?}");
        assert_eq!(
            updated.stdout,
            "updated remote-tool from 0.1.0 to 0.2.0 via registry\n"
        );
        assert_eq!(session.execute_line("remote-tool").stdout, "remote-v2\n");
        assert_eq!(
            session
                .history()
                .iter()
                .filter(|entry| entry.as_str() == "[redacted network command]")
                .count(),
            2
        );

        let recorded = requests.lock().expect("request log lock");
        assert_eq!(recorded.len(), 7);
        assert!(recorded
            .iter()
            .all(|request| request.method == NetworkMethod::Get));
        assert!(recorded
            .iter()
            .all(|request| request.headers
                == [("Accept".to_string(), "application/json".to_string())]));
        drop(recorded);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn rejects_remote_package_artifacts_outside_the_registry_origin() {
        let root = test_root();
        let index_url = "https://registry.example.test/index.json";
        let index = br#"{
            "schema_version": 1,
            "packages": [{
                "name": "remote-tool",
                "version": "0.1.0",
                "description": "A remote Rune tool",
                "manifest_url": "https://cdn.example.test/manifest.json",
                "artifacts": [{"path": "bin/remote.rune", "url": "https://cdn.example.test/remote.rune"}]
            }]
        }"#;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_network_provider(Box::new(RoutingNetworkProvider {
            requests: Arc::clone(&requests),
            routes: vec![(
                index_url.to_string(),
                NetworkResponse {
                    status_code: 200,
                    body: index.to_vec(),
                },
            )],
        }));
        let rejected = session.execute_line(&format!(
            "pkg install --registry {index_url} remote-tool 0.1.0"
        ));
        assert_eq!(rejected.status, 2);
        assert!(rejected.stderr.contains("registry origin"));
        assert_eq!(requests.lock().expect("request log lock").len(), 1);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn keeps_the_installed_remote_version_when_a_new_artifact_is_tampered() {
        let root = test_root();
        let index_url = "https://registry.example.test/index.json";
        let manifest_v1_url = "https://registry.example.test/tool/v1/manifest.json";
        let artifact_v1_url = "https://registry.example.test/tool/v1/bin/tool.rune";
        let manifest_v2_url = "https://registry.example.test/tool/v2/manifest.json";
        let artifact_v2_url = "https://registry.example.test/tool/v2/bin/tool.rune";
        let script_v1 = b"echo stable\n";
        let expected_v2 = b"echo verified\n";
        let manifest_v1 = format!(
            r#"{{"schema_version":1,"name":"remote-tool","version":"0.1.0","description":"Remote tool","files":[{{"path":"bin/tool.rune","sha256":"{}"}}],"commands":[{{"name":"remote-tool","entry":"bin/tool.rune"}}]}}"#,
            rune_package::sha256_hex(script_v1)
        );
        let manifest_v2 = format!(
            r#"{{"schema_version":1,"name":"remote-tool","version":"0.2.0","description":"Remote tool","files":[{{"path":"bin/tool.rune","sha256":"{}"}}],"commands":[{{"name":"remote-tool","entry":"bin/tool.rune"}}]}}"#,
            rune_package::sha256_hex(expected_v2)
        );
        let index = format!(
            r#"{{"schema_version":1,"packages":[
                {{"name":"remote-tool","version":"0.1.0","description":"Remote tool","manifest_url":"{manifest_v1_url}","artifacts":[{{"path":"bin/tool.rune","url":"{artifact_v1_url}"}}]}},
                {{"name":"remote-tool","version":"0.2.0","description":"Remote tool","manifest_url":"{manifest_v2_url}","artifacts":[{{"path":"bin/tool.rune","url":"{artifact_v2_url}"}}]}}
            ]}}"#
        );
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.set_network_provider(Box::new(RoutingNetworkProvider {
            requests: Arc::new(Mutex::new(Vec::new())),
            routes: vec![
                (
                    index_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: index.into_bytes(),
                    },
                ),
                (
                    manifest_v1_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: manifest_v1.into_bytes(),
                    },
                ),
                (
                    artifact_v1_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: script_v1.to_vec(),
                    },
                ),
                (
                    manifest_v2_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: manifest_v2.into_bytes(),
                    },
                ),
                (
                    artifact_v2_url.to_string(),
                    NetworkResponse {
                        status_code: 200,
                        body: b"tampered\n".to_vec(),
                    },
                ),
            ],
        }));
        assert_eq!(
            session
                .execute_line(&format!(
                    "pkg install --registry {index_url} remote-tool 0.1.0"
                ))
                .status,
            0
        );
        let rejected = session.execute_line(&format!(
            "pkg update --registry {index_url} remote-tool 0.2.0"
        ));
        assert_eq!(rejected.status, 1);
        assert!(rejected.stderr.contains("integrity mismatch"));
        assert_eq!(session.execute_line("remote-tool").stdout, "stable\n");
        assert_eq!(
            session.execute_line("pkg list").stdout,
            "remote-tool@0.1.0\n"
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn keeps_named_session_state_independent_and_bounded() {
        let root = test_root();
        let mut first = Session::restore_with_id(
            SandboxedFileSystem::new(&root).expect("root created"),
            "first",
        )
        .expect("valid session id");
        assert_eq!(first.execute_line("mkdir first-dir").status, 0);
        assert_eq!(first.execute_line("cd first-dir").status, 0);
        assert_eq!(first.execute_line("echo first-session").status, 0);
        first.persist().expect("first session persisted");

        let mut second = Session::restore_with_id(
            SandboxedFileSystem::new(&root).expect("root reopened"),
            "second",
        )
        .expect("valid session id");
        assert_eq!(second.current_directory(), "~");
        assert!(!second
            .history()
            .iter()
            .any(|command| command.contains("first-session")));
        assert_eq!(second.execute_line("echo second-session").status, 0);
        second.persist().expect("second session persisted");

        let restored_first = Session::restore_with_id(
            SandboxedFileSystem::new(&root).expect("root reopened again"),
            "first",
        )
        .expect("first session restored");
        assert_eq!(restored_first.current_directory(), "~/first-dir");
        assert!(restored_first
            .history()
            .iter()
            .any(|command| command.contains("first-session")));
        assert!(Session::restore_with_id(
            SandboxedFileSystem::new(&root).expect("root reopened for invalid id"),
            "../escape",
        )
        .is_err());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn transfers_bounded_files_through_the_confined_session_filesystem() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("mkdir transfer").status, 0);
        session
            .write_file("transfer/note.txt", b"from automation\n")
            .expect("file written through VFS");
        assert_eq!(
            session
                .read_file("transfer/note.txt")
                .expect("file read through VFS"),
            b"from automation\n"
        );
        assert!(session.read_file("../outside").is_err());
        let oversized = vec![b'x'; MAX_FILE_TRANSFER_BYTES + 1];
        assert!(session
            .write_file("transfer/too-large", &oversized)
            .is_err());
        assert!(!root.join("transfer/too-large").exists());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn creates_and_extracts_a_bounded_zip_archive() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("mkdir -p source/nested").status, 0);
        assert_eq!(
            session
                .execute_line("echo archive-data > source/nested/note.txt")
                .status,
            0
        );
        assert_eq!(session.execute_line("touch source/empty.txt").status, 0);
        let created = session.execute_line("zip -r bundle.zip source");
        assert_eq!(created.status, 0);
        assert!(created.stdout.contains("4 entries"));
        assert!(root.join("bundle.zip").exists());

        let listing = session.execute_line("unzip -l bundle.zip source/nested");
        assert_eq!(listing.status, 0, "{listing:?}");
        assert!(listing.stdout.contains("source/nested/note.txt\n"));
        assert!(!listing.stdout.contains("source/empty.txt\n"));

        assert_eq!(session.execute_line("rm -r source").status, 0);
        let extracted = session.execute_line("unzip bundle.zip restored");
        assert_eq!(extracted.status, 0);
        assert_eq!(
            session
                .execute_line("cat restored/source/nested/note.txt")
                .stdout,
            "archive-data\n"
        );
        assert_eq!(
            session
                .execute_line("stat restored/source/empty.txt")
                .status,
            0
        );
        let filtered = session.execute_line("unzip bundle.zip -d filtered source/nested");
        assert_eq!(filtered.status, 0, "{filtered:?}");
        assert!(filtered.stdout.contains("2 entries"));
        assert_eq!(
            session
                .execute_line("cat filtered/source/nested/note.txt")
                .stdout,
            "archive-data\n"
        );
        assert_eq!(
            session
                .execute_line("test -e filtered/source/empty.txt")
                .status,
            1
        );
        let missing = session.execute_line("unzip bundle.zip -d missing does-not-exist");
        assert_eq!(missing.status, 1, "{missing:?}");
        assert!(!root.join("missing").exists());
        assert_eq!(
            session.execute_line("unzip bundle.zip ../outside").status,
            1
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn creates_lists_and_extracts_a_bounded_ar_archive() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let first = b"object-one\0".to_vec();
        let second = b"object-two".to_vec();
        let long_name = "a-long-object-member.o";
        let long = b"object-long".to_vec();
        std::fs::write(root.join("first.o"), &first).expect("first object written");
        std::fs::write(root.join("second.o"), &second).expect("second object written");
        std::fs::write(root.join(long_name), &long).expect("long object written");

        let created =
            session.execute_line("ar -rcs bundle.a first.o second.o a-long-object-member.o");
        assert_eq!(created.status, 0);
        assert!(root.join("bundle.a").exists());
        let listing = session.execute_line("ar t bundle.a");
        assert_eq!(listing.status, 0);
        assert_eq!(
            listing.stdout,
            "first.o\nsecond.o\na-long-object-member.o\n"
        );
        let filtered_listing = session.execute_line("ar t bundle.a second.o");
        assert_eq!(filtered_listing.status, 0, "{filtered_listing:?}");
        assert_eq!(filtered_listing.stdout, "second.o\n");
        let filtered_long = session.execute_line("ar t bundle.a a-long-object-member.o");
        assert_eq!(filtered_long.status, 0, "{filtered_long:?}");
        assert_eq!(filtered_long.stdout, "a-long-object-member.o\n");
        assert_eq!(session.execute_line("ar t bundle.a missing.o").status, 1);

        assert_eq!(
            session
                .execute_line("rm first.o second.o a-long-object-member.o")
                .status,
            0
        );
        let extracted = session.execute_line("ar x bundle.a");
        assert_eq!(extracted.status, 0);
        assert_eq!(
            std::fs::read(root.join("first.o")).expect("first output read"),
            first
        );
        assert_eq!(
            std::fs::read(root.join("second.o")).expect("second output read"),
            second
        );
        assert_eq!(
            std::fs::read(root.join(long_name)).expect("long output read"),
            long
        );
        assert_eq!(session.execute_line("ar x bundle.a").status, 1);
        assert_eq!(session.execute_line("rm first.o").status, 0);
        let blocked = session.execute_line("ar x bundle.a");
        assert_eq!(blocked.status, 1, "{blocked:?}");
        assert!(!root.join("first.o").exists());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_xargs_batches_through_the_rust_planner() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let batched = session.execute_line("printf 'one two three' | xargs -n 2 echo item");
        assert_eq!(batched.status, 0);
        assert_eq!(batched.stdout, "item one two\nitem three\n");

        let skipped = session.execute_line("printf '' | xargs -r echo should-not-run");
        assert_eq!(skipped.status, 0);
        assert!(skipped.stdout.is_empty());

        let quoted = session.execute_line("printf 'safe;value' | xargs echo");
        assert_eq!(quoted.status, 0);
        assert_eq!(quoted.stdout, "safe;value\n");
        std::fs::write(root.join("null-items"), b"one\0two\0three\0")
            .expect("null-delimited input written");
        let null_delimited = session.execute_line("cat null-items | xargs -0 -n 2 echo");
        assert_eq!(null_delimited.status, 0);
        assert_eq!(null_delimited.stdout, "one two\nthree\n");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn evaluates_bounded_command_substitutions_in_an_isolated_shell_state() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session.execute_line("echo value=$(echo nested)").stdout,
            "value=nested\n"
        );
        assert_eq!(
            session.execute_line("echo nested=$(echo $(pwd))").stdout,
            "nested=~\n"
        );
        assert_eq!(session.execute_line("mkdir child").status, 0);
        assert_eq!(
            session.execute_line("echo inside=$(cd child; pwd)").stdout,
            "inside=~/child\n"
        );
        assert_eq!(session.execute_line("pwd").stdout, "~\n");
        assert_eq!(
            session
                .execute_line("VALUE=$(printf result); echo $VALUE")
                .stdout,
            "result\n"
        );
        assert_eq!(
            session
                .execute_line("echo data > $(echo output.txt)")
                .status,
            0
        );
        assert_eq!(session.execute_line("cat output.txt").stdout, "data\n");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn compresses_and_decompresses_bounded_gzip_files() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("echo gzip-data > note.txt").status, 0);
        let compressed = session.execute_line("gzip note.txt");
        assert_eq!(compressed.status, 0);
        assert!(compressed.stdout.contains("note.txt -> note.txt.gz"));
        assert!(root.join("note.txt.gz").exists());
        assert!(root.join("note.txt").exists());

        assert_eq!(session.execute_line("rm note.txt").status, 0);
        let decompressed = session.execute_line("gunzip note.txt.gz");
        assert_eq!(decompressed.status, 0);
        assert_eq!(session.execute_line("cat note.txt").stdout, "gzip-data\n");

        assert_eq!(session.execute_line("gzip -c note.txt").status, 2);
        assert_eq!(session.execute_line("gunzip note.txt").status, 1);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn compresses_and_decompresses_bounded_lzw_files() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let source: Vec<u8> = (0_usize..20_000)
            .map(|index| u8::try_from(index % 251).expect("pattern fits in a byte"))
            .collect();
        std::fs::write(root.join("payload.bin"), &source).expect("source written");

        let compressed = session.execute_line("compress payload.bin");
        assert_eq!(compressed.status, 0);
        assert!(compressed.stdout.contains("payload.bin -> payload.bin.Z"));
        assert!(root.join("payload.bin.Z").exists());
        assert_eq!(session.execute_line("rm payload.bin").status, 0);

        let decompressed = session.execute_line("uncompress payload.bin.Z");
        assert_eq!(decompressed.status, 0, "{decompressed:?}");
        assert_eq!(
            std::fs::read(root.join("payload.bin")).expect("output read"),
            source
        );
        assert_eq!(session.execute_line("uncompress payload.bin").status, 1);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn processes_bounded_awk_fields_and_patterns() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let fields = session.execute_line("printf 'red 1\\nblue 2\\n' | awk '{print $2}'");
        assert_eq!(fields.status, 0);
        assert_eq!(fields.stdout, "1\n2\n");

        assert_eq!(
            session
                .execute_line("printf 'ok,1,red\\nskip,2,blue\\n' > rows.csv")
                .status,
            0
        );
        let selected = session
            .execute_line("awk -F, 'BEGIN { OFS = \":\" } $2 == \"1\" { print $1, $3 }' rows.csv");
        assert_eq!(selected.status, 0);
        assert_eq!(selected.stdout, "ok:red\n");

        let regex = session.execute_line("awk -F, '$1 ~ /^o/ { print $1 }' rows.csv");
        assert_eq!(regex.status, 0);
        assert_eq!(regex.stdout, "ok\n");
        let record_regex = session.execute_line("awk '/^skip/ { print $1 }' rows.csv");
        assert_eq!(record_regex.status, 0);
        assert_eq!(record_regex.stdout, "skip,2,blue\n");

        let ended = session.execute_line("awk 'END { print \"done\" }' rows.csv");
        assert_eq!(ended.status, 0);
        assert_eq!(ended.stdout, "done\n");
        assert_eq!(session.execute_line("awk -F, 'next' rows.csv").status, 2);
        let invalid_regex = session.execute_line("awk '/[/{ print }' rows.csv");
        assert_eq!(invalid_regex.status, 2);
        assert!(invalid_regex.stderr.contains("invalid regular expression"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn creates_lists_and_extracts_a_bounded_ustar_archive() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("mkdir -p source/nested").status, 0);
        assert_eq!(
            session
                .execute_line("echo tar-data > source/nested/note.txt")
                .status,
            0
        );
        assert_eq!(session.execute_line("touch source/empty.txt").status, 0);

        let created = session.execute_line("tar -cf bundle.tar source");
        assert_eq!(created.status, 0);
        assert!(created.stdout.contains("4 entries"));
        assert!(root.join("bundle.tar").exists());

        let listing = session.execute_line("tar -tf bundle.tar");
        assert_eq!(listing.status, 0);
        assert!(listing.stdout.contains("source/\n"));
        assert!(listing.stdout.contains("source/nested/note.txt\n"));
        let filtered_listing = session.execute_line("tar -tf bundle.tar source/nested");
        assert_eq!(filtered_listing.status, 0, "{filtered_listing:?}");
        assert!(filtered_listing.stdout.contains("source/nested/note.txt\n"));
        assert!(!filtered_listing.stdout.contains("source/empty.txt\n"));
        assert_eq!(
            session
                .execute_line("tar -tf bundle.tar does-not-exist")
                .status,
            1
        );

        let compressed = session.execute_line("tar -czf compressed.tar source");
        assert_eq!(compressed.status, 0, "{compressed:?}");
        assert!(root.join("compressed.tar").exists());
        let compressed_listing = session.execute_line("tar -tzf compressed.tar");
        assert_eq!(compressed_listing.status, 0, "{compressed_listing:?}");
        assert!(compressed_listing
            .stdout
            .contains("source/nested/note.txt\n"));

        assert_eq!(session.execute_line("rm -r source").status, 0);
        let filtered_extracted =
            session.execute_line("tar -xf bundle.tar source/nested/note.txt -C filtered");
        assert_eq!(filtered_extracted.status, 0, "{filtered_extracted:?}");
        assert_eq!(
            session
                .execute_line("cat filtered/source/nested/note.txt")
                .stdout,
            "tar-data\n"
        );
        assert_eq!(
            session
                .execute_line("test -e filtered/source/empty.txt")
                .status,
            1
        );
        let extracted = session.execute_line("tar -xf bundle.tar -C restored");
        assert_eq!(extracted.status, 0);
        assert_eq!(
            session
                .execute_line("cat restored/source/nested/note.txt")
                .stdout,
            "tar-data\n"
        );
        let compressed_extracted = session.execute_line("tar -xzf compressed.tar -C restored-gzip");
        assert_eq!(compressed_extracted.status, 0, "{compressed_extracted:?}");
        assert_eq!(
            session
                .execute_line("cat restored-gzip/source/nested/note.txt")
                .stdout,
            "tar-data\n"
        );
        assert_eq!(
            session
                .execute_line("tar -xzf bundle.tar -C invalid")
                .status,
            1
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn provides_bounded_rust_owned_command_and_path_completion() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.completion_candidates("ec"), vec!["echo"]);
        assert_eq!(
            session.completion_candidates("  pr"),
            vec!["printenv", "printf"]
        );
        assert_eq!(session.execute_line("unlisted-command").status, 127);
        assert_eq!(
            session.completion_candidates("unlisted"),
            vec!["unlisted-command"]
        );
        assert_eq!(session.execute_line("alias personal=echo").status, 0);
        assert_eq!(session.completion_candidates("pers"), vec!["personal"]);
        assert_eq!(
            session.apply_completion("ec", "echo"),
            Some("echo ".to_string())
        );
        assert!(session.completion_candidates("echo ").is_empty());
        assert_eq!(
            session.completion_candidates("ec | ca"),
            vec!["call", "cat"]
        );
        assert_eq!(
            session.completion_candidates("echo | ca"),
            vec!["call", "cat"]
        );
        assert!(session.completion_candidates("echo || ca").is_empty());
        assert!(session.completion_candidates("echo | ca | pu").is_empty());
        assert!(session
            .completion_candidates(&"e".repeat(64 * 1024 + 1))
            .is_empty());
        assert_eq!(session.execute_line("mkdir docs").status, 0);
        assert_eq!(session.execute_line("echo notes > docs/notes.md").status, 0);
        assert_eq!(session.completion_candidates("cat do"), vec!["docs/"]);
        assert_eq!(
            session.apply_completion("cat do", "docs/"),
            Some("cat docs/".to_string())
        );
        assert_eq!(
            session.apply_completion("cat ~/do", "~/docs/"),
            Some("cat ~/docs/".to_string())
        );
        assert_eq!(session.execute_line("cd docs").status, 0);
        assert_eq!(session.execute_line("bookmark project").status, 0);
        assert_eq!(session.execute_line("cd ~").status, 0);
        assert_eq!(session.completion_candidates("z do"), vec!["docs/"]);
        assert_eq!(session.completion_candidates("cd ~pro"), vec!["~project/"]);
        assert_eq!(
            session.apply_completion("cd ~pro", "~project/"),
            Some("cd ~project/".to_string())
        );
        assert_eq!(
            session.completion_candidates("cat ~project/no"),
            vec!["~project/notes.md"]
        );
        assert_eq!(
            session.apply_completion("cat ~project/no", "~project/notes.md"),
            Some("cat ~project/notes.md ".to_string())
        );
        assert_eq!(
            session.completion_candidates("echo | cat do"),
            vec!["docs/"]
        );
        assert_eq!(
            session.apply_completion("echo | ca", "cat"),
            Some("echo | cat ".to_string())
        );
        assert!(session.apply_completion("cat \"no", "docs/").is_none());
        assert!(session.apply_completion("cat do", "missing").is_none());
        assert_eq!(
            session.completion_candidates("cat docs/n"),
            vec!["docs/notes.md"]
        );
        assert_eq!(
            session.completion_candidates("cat docs/"),
            vec!["docs/notes.md"]
        );
        assert_eq!(session.completion_candidates("cat ~/do"), vec!["~/docs/"]);
        assert_eq!(session.completion_candidates("cat ./do"), vec!["./docs/"]);
        assert_eq!(session.completion_candidates("cd do"), vec!["docs/"]);
        assert_eq!(session.completion_candidates("echo > do"), vec!["docs/"]);
        assert_eq!(session.completion_candidates("echo < "), vec!["docs/"]);
        assert_eq!(session.completion_candidates("cat -"), Vec::<String>::new());
        assert!(session.completion_candidates("echo no").is_empty());
        assert!(session.completion_candidates("cat \"no").is_empty());
        for index in 0..10 {
            std::fs::write(root.join(format!("candidate-{index:02}.txt")), b"candidate")
                .expect("completion candidate written");
        }
        assert_eq!(session.completion_candidates("cat ").len(), 8);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn persists_and_applies_a_bounded_history_configuration() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.configuration().history_limit(), 1_000);
        assert_eq!(session.configuration().scrollback_limit(), 4_096);
        assert!(session.configuration().toolbar_visible());
        assert_eq!(session.execute_line("config set history-limit 3").status, 0);
        assert_eq!(
            session.execute_line("config get history-limit").stdout,
            "history-limit=3\n"
        );
        assert_eq!(session.execute_line("config set font-size 20").status, 0);
        assert_eq!(
            session.execute_line("config get font-size").stdout,
            "font-size=20\n"
        );
        assert_eq!(session.execute_line("config set theme ember").status, 0);
        assert_eq!(
            session.execute_line("config get theme").stdout,
            "theme=ember\n"
        );
        assert_eq!(
            session
                .execute_line("config set scrollback-limit 2048")
                .status,
            0
        );
        assert_eq!(
            session.execute_line("config get scrollback-limit").stdout,
            "scrollback-limit=2048\n"
        );
        assert_eq!(
            session
                .execute_line("config set toolbar-visible false")
                .status,
            0
        );
        assert_eq!(
            session.execute_line("config get toolbar-visible").stdout,
            "toolbar-visible=false\n"
        );
        assert_eq!(
            session.execute_line("config get history-redaction").stdout,
            "history-redaction=true\n"
        );
        assert_eq!(
            session
                .execute_line("config set history-redaction off")
                .status,
            0
        );
        assert!(!session.configuration().history_redaction());
        assert_eq!(
            session.execute_line("config get history-redaction").stdout,
            "history-redaction=false\n"
        );
        assert_eq!(session.execute_line("echo one").status, 0);
        assert_eq!(session.execute_line("echo two").status, 0);
        assert!(session.history().len() <= 3);
        assert_eq!(session.execute_line("config set history-limit 0").status, 2);
        assert_eq!(session.configuration().history_limit(), 3);
        assert_eq!(session.execute_line("config set font-size 33").status, 2);
        assert_eq!(session.configuration().font_size(), 20);
        assert_eq!(session.execute_line("config set theme paper").status, 2);
        assert_eq!(session.configuration().theme().as_str(), "ember");
        assert_eq!(
            session
                .execute_line("config set scrollback-limit 127")
                .status,
            2
        );
        assert_eq!(session.configuration().scrollback_limit(), 2_048);
        assert_eq!(
            session
                .execute_line("config set scrollback-limit 8193")
                .status,
            2
        );
        assert_eq!(session.configuration().scrollback_limit(), 2_048);
        session.persist().expect("configuration persisted");
        let mut restored =
            Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert_eq!(restored.configuration().history_limit(), 3);
        assert_eq!(restored.configuration().font_size(), 20);
        assert_eq!(restored.configuration().scrollback_limit(), 2_048);
        assert!(!restored.configuration().toolbar_visible());
        assert_eq!(restored.configuration().theme().as_str(), "ember");
        assert!(!restored.configuration().history_redaction());
        assert!(restored.history().len() <= 3);
        assert_eq!(restored.execute_line("config reset").status, 0);
        assert_eq!(restored.configuration().history_limit(), 1_000);
        assert_eq!(restored.configuration().scrollback_limit(), 4_096);
        assert!(restored.configuration().toolbar_visible());
        assert!(restored.configuration().history_redaction());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn exposes_toolbar_visibility_commands_through_persisted_configuration() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert!(session.configuration().toolbar_visible());
        assert_eq!(session.execute_line("hideToolbar").status, 0);
        assert!(!session.configuration().toolbar_visible());
        assert_eq!(session.execute_line("showToolbar").status, 0);
        assert!(session.configuration().toolbar_visible());
        assert_eq!(session.execute_line("hideToolbar").status, 0);
        let invalid = session.execute_line("showToolbar extra");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("usage: showToolbar"));
        session.persist().expect("toolbar visibility persisted");
        let restored = Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert!(!restored.configuration().toolbar_visible());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn persists_and_applies_terminal_appearance_configuration() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let settings = [
            ("font", "rounded", "font=rounded\n"),
            ("cursor-color", "ember", "cursor-color=ember\n"),
            ("cursor-shape", "underline", "cursor-shape=underline\n"),
            ("background", "slate", "background=slate\n"),
            ("foreground", "ember", "foreground=ember\n"),
        ];
        for (key, value, expected) in settings {
            assert_eq!(
                session
                    .execute_line(&format!("config set {key} {value}"))
                    .status,
                0
            );
            assert_eq!(
                session.execute_line(&format!("config get {key}")).stdout,
                expected
            );
        }
        session.persist().expect("appearance persisted");
        let mut restored =
            Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert_eq!(restored.configuration().font().as_str(), "rounded");
        assert_eq!(restored.configuration().cursor_color().as_str(), "ember");
        assert_eq!(
            restored.configuration().cursor_shape().as_str(),
            "underline"
        );
        assert_eq!(restored.configuration().background().as_str(), "slate");
        assert_eq!(restored.configuration().foreground().as_str(), "ember");
        assert_eq!(restored.execute_line("config reset").status, 0);
        assert_eq!(restored.configuration(), &TerminalConfig::default());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn native_configuration_updates_share_validation_without_history_entries() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("echo keep").status, 0);
        let changed = session.set_configuration("font-size", "20");
        assert_eq!(changed.status, 0);
        assert_eq!(session.configuration().font_size(), 20);
        assert_eq!(
            session.set_configuration("toolbar-visible", "false").status,
            0
        );
        assert!(!session.configuration().toolbar_visible());
        assert_eq!(
            session
                .set_configuration("cursor-color", "foreground")
                .status,
            0
        );
        assert_eq!(
            session.configuration().cursor_color().as_str(),
            "foreground"
        );
        assert_eq!(session.set_configuration("cursor-shape", "block").status, 0);
        assert_eq!(session.configuration().cursor_shape().as_str(), "block");
        assert_eq!(session.set_configuration("font", "system").status, 0);
        assert_eq!(session.configuration().font().as_str(), "system");
        assert_eq!(session.set_configuration("background", "white").status, 0);
        assert_eq!(session.configuration().background().as_str(), "white");
        assert_eq!(session.set_configuration("foreground", "black").status, 0);
        assert_eq!(session.configuration().foreground().as_str(), "black");
        assert_eq!(
            session.set_configuration("history-redaction", "off").status,
            0
        );
        assert!(!session.configuration().history_redaction());
        assert_eq!(
            session.set_configuration("history-redaction", "on").status,
            0
        );
        assert!(session.configuration().history_redaction());
        assert_eq!(session.history(), ["echo keep"]);

        let invalid = session.set_configuration("theme", "paper");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("theme must be one of"));
        assert_eq!(session.configuration().theme().as_str(), "ink");
        let invalid_cursor = session.set_configuration("cursor-color", "violet");
        assert_eq!(invalid_cursor.status, 2);
        assert!(invalid_cursor
            .stderr
            .contains("cursor-color must be one of"));
        assert_eq!(
            session.configuration().cursor_color().as_str(),
            "foreground"
        );
        let invalid_cursor_shape = session.set_configuration("cursor-shape", "diamond");
        assert_eq!(invalid_cursor_shape.status, 2);
        assert!(invalid_cursor_shape
            .stderr
            .contains("cursor-shape must be one of"));
        assert_eq!(session.configuration().cursor_shape().as_str(), "block");
        let invalid_font = session.set_configuration("font", "serif");
        assert_eq!(invalid_font.status, 2);
        assert!(invalid_font.stderr.contains("font must be one of"));
        assert_eq!(session.configuration().font().as_str(), "system");
        let invalid_background = session.set_configuration("background", "purple");
        assert_eq!(invalid_background.status, 2);
        assert!(invalid_background
            .stderr
            .contains("background must be one of"));
        assert_eq!(session.configuration().background().as_str(), "white");
        let invalid_foreground = session.set_configuration("foreground", "purple");
        assert_eq!(invalid_foreground.status, 2);
        assert!(invalid_foreground
            .stderr
            .contains("foreground must be one of"));
        assert_eq!(session.configuration().foreground().as_str(), "black");

        assert_eq!(session.set_configuration("history-limit", "1").status, 0);
        assert_eq!(session.history(), ["echo keep"]);
        assert_eq!(session.reset_configuration().status, 0);
        assert_eq!(session.configuration(), &TerminalConfig::default());
        assert_eq!(session.history(), ["echo keep"]);
        assert_eq!(session.execute_line("echo after-reset").status, 0);
        assert_eq!(session.history(), ["echo keep", "echo after-reset"]);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn configures_history_redaction_with_safe_default_and_explicit_opt_out() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert!(session.configuration().history_redaction());
        assert_eq!(
            session
                .execute_line("export DEFAULT_SAFE_TOKEN=hidden-by-default")
                .status,
            0
        );
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted environment assignment]")
        );

        let opted_out = session.set_configuration("history-redaction", "false");
        assert_eq!(opted_out.status, 0);
        assert!(!session.configuration().history_redaction());
        assert_eq!(
            session
                .execute_line("export OPT_OUT_TOKEN=visible-by-choice")
                .status,
            0
        );
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("export OPT_OUT_TOKEN=visible-by-choice")
        );
        session.persist().expect("configuration persisted");

        let mut restored =
            Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert!(!restored.configuration().history_redaction());
        assert_eq!(
            restored.history().last().map(String::as_str),
            Some("export OPT_OUT_TOKEN=visible-by-choice")
        );
        let invalid = restored.set_configuration("history-redaction", "maybe");
        assert_eq!(invalid.status, 2);
        assert!(invalid
            .stderr
            .contains("history-redaction must be true or false"));
        assert!(!restored.configuration().history_redaction());

        assert_eq!(
            restored
                .set_configuration("history-redaction", "true")
                .status,
            0
        );
        assert_eq!(
            restored
                .execute_line("export REENABLED_TOKEN=hidden-again")
                .status,
            0
        );
        assert_eq!(
            restored.history().last().map(String::as_str),
            Some("[redacted environment assignment]")
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn persists_user_environment_only_after_explicit_opt_in() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert!(!session.configuration().environment_persistence());
        assert_eq!(
            session.execute_line("export RUNE_EDITOR=rune-edit").status,
            0
        );
        session.persist().expect("default state persisted");
        let state_without_environment = std::fs::read_to_string(root.join(".rune/session.state"))
            .expect("session state readable");
        assert!(!state_without_environment.contains("environment="));
        let restored_without_environment =
            Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert!(!restored_without_environment
            .environment()
            .contains_key("RUNE_EDITOR"));

        assert_eq!(
            session
                .set_configuration("environment-persistence", "true")
                .status,
            0
        );
        assert_eq!(
            session.execute_line("export RUNE_PAGER=rune-page").status,
            0
        );
        assert_eq!(
            session
                .execute_line("export HOME=~/should-not-persist")
                .status,
            0
        );
        session.persist().expect("opted-in state persisted");
        let state_with_environment = std::fs::read_to_string(root.join(".rune/session.state"))
            .expect("opted-in session state readable");
        assert!(state_with_environment.contains("environment=RUNE_EDITOR\trune-edit"));
        assert!(state_with_environment.contains("environment=RUNE_PAGER\trune-page"));
        assert!(!state_with_environment.contains("should-not-persist"));
        let restored_with_environment =
            Session::restore(SandboxedFileSystem::new(&root).expect("root reopened again"));
        assert!(restored_with_environment
            .configuration()
            .environment_persistence());
        assert_eq!(
            restored_with_environment.environment().get("RUNE_EDITOR"),
            Some(&"rune-edit".to_string())
        );
        assert_eq!(
            restored_with_environment.environment().get("RUNE_PAGER"),
            Some(&"rune-page".to_string())
        );
        assert_eq!(
            restored_with_environment.environment().get("HOME"),
            Some(&"~".to_string())
        );

        let mut opted_out = restored_with_environment;
        assert_eq!(
            opted_out
                .set_configuration("environment-persistence", "false")
                .status,
            0
        );
        opted_out.persist().expect("opted-out state persisted");
        let state_after_opt_out = std::fs::read_to_string(root.join(".rune/session.state"))
            .expect("opted-out session state readable");
        assert!(!state_after_opt_out.contains("environment="));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn limits_and_clears_session_history_through_the_builtin() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("echo first").status, 0);
        assert_eq!(session.execute_line("echo second").status, 0);
        let recent = session.execute_line("history 1");
        assert_eq!(recent.status, 0);
        assert!(recent.stdout.contains("history 1"));
        assert!(!recent.stdout.contains("echo second"));
        assert_eq!(session.execute_line("history -c").status, 0);
        assert!(session.history().is_empty());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn searches_history_case_insensitively_with_original_entry_numbers() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("echo Alpha").status, 0);
        assert_eq!(session.execute_line("pwd").status, 0);

        let search = session.execute_line("history search ALPHA");
        assert_eq!(search.status, 0);
        assert_eq!(
            search.stdout,
            "    1  echo Alpha\n    3  history search ALPHA\n"
        );

        let joined_query = session.execute_line("history search echo Alpha");
        assert_eq!(joined_query.status, 0);
        assert_eq!(
            joined_query.stdout,
            "    1  echo Alpha\n    4  history search echo Alpha\n"
        );

        let empty_query = session.execute_line("history search \"\"");
        assert_eq!(empty_query.status, 2);
        assert!(empty_query
            .stderr
            .contains("history: search query must contain"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn searches_history_from_newest_without_mutating_history() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("echo Alpha").status, 0);
        assert_eq!(session.execute_line("echo other").status, 0);
        assert_eq!(session.execute_line("echo alpha-two").status, 0);
        let before = session.history().to_vec();

        assert_eq!(
            session.search_history("ALPHA"),
            Some(vec!["echo alpha-two".to_string(), "echo Alpha".to_string()])
        );
        assert_eq!(session.history(), before.as_slice());
        assert_eq!(session.search_history("missing"), Some(Vec::new()));
        assert!(session.search_history(&"x".repeat(4 * 1024 + 1)).is_none());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn suppresses_only_consecutive_duplicate_history_entries() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("echo repeat").status, 0);
        assert_eq!(session.execute_line("echo repeat").status, 0);
        assert_eq!(session.execute_line("echo other").status, 0);
        assert_eq!(session.execute_line("echo repeat").status, 0);
        assert_eq!(
            session.history(),
            ["echo repeat", "echo other", "echo repeat"]
        );

        session.persist().expect("deduplicated history persisted");
        let restored = Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert_eq!(
            restored.history(),
            ["echo repeat", "echo other", "echo repeat"]
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn tracks_previous_directory_and_prints_it_for_cd_dash() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("mkdir work").status, 0);

        assert_eq!(session.execute_line("cd work").status, 0);
        assert_eq!(session.current_directory(), "~/work");
        assert_eq!(
            session.environment().get("PWD"),
            Some(&"~/work".to_string())
        );
        assert_eq!(session.environment().get("OLDPWD"), Some(&"~".to_string()));

        let previous = session.execute_line("cd -");
        assert_eq!(previous.status, 0);
        assert_eq!(previous.stdout, "~\n");
        assert_eq!(session.current_directory(), "~");
        assert_eq!(session.environment().get("PWD"), Some(&"~".to_string()));
        assert_eq!(
            session.environment().get("OLDPWD"),
            Some(&"~/work".to_string())
        );

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn bounds_automation_input_and_accumulated_output() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let too_many_lines = "true\n".repeat(MAX_SCRIPT_LINES + 1);
        let line_limit = session.execute_script(&too_many_lines);
        assert_eq!(line_limit.status, 2);
        assert!(line_limit.stderr.contains("line input limit"));
        assert!(session.history().is_empty());

        let too_many_bytes = "x".repeat(MAX_SCRIPT_BYTES + 1);
        let byte_limit = session.execute_script(&too_many_bytes);
        assert_eq!(byte_limit.status, 2);
        assert!(byte_limit.stderr.contains("byte input limit"));
        assert!(session.history().is_empty());

        std::fs::write(
            root.join("large.txt"),
            vec![b'x'; MAX_OUTPUT_BYTES + OUTPUT_TRUNCATION_MARKER.len() + 128],
        )
        .expect("large file written");
        let output = session.execute_script("cat large.txt\ncat large.txt");
        assert_eq!(output.status, 0);
        assert!(output.stdout.len() <= MAX_OUTPUT_BYTES);
        assert!(output.stdout.ends_with(OUTPUT_TRUNCATION_MARKER));
        assert!(output.stderr.len() <= MAX_OUTPUT_BYTES);
        assert_eq!(session.history(), ["cat large.txt"]);

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn sources_bounded_scripts_through_the_virtual_filesystem() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        std::fs::write(
            root.join("env.rc"),
            b"export FROM_SOURCE=yes\nalias say='echo sourced'\nsource nested.rc\n",
        )
        .expect("source file written");
        std::fs::write(root.join("nested.rc"), b"echo nested\n").expect("nested file written");
        std::fs::write(root.join("args.rc"), b"echo $0 $1 $2 $# $@\n")
            .expect("argument script written");
        std::fs::write(root.join("stdin.rc"), b"cat -\n").expect("stdin script written");
        std::fs::write(
            root.join("outer.rc"),
            b"source args.rc inner\necho outer:$1\n",
        )
        .expect("nested argument script written");

        let sourced = session.execute_line("source env.rc");
        assert_eq!(sourced.status, 0);
        assert_eq!(sourced.stdout, "nested\n");
        assert_eq!(
            session.environment().get("FROM_SOURCE"),
            Some(&"yes".to_string())
        );
        assert_eq!(session.execute_line("say").stdout, "sourced\n");
        assert!(session.history().contains(&"source env.rc".to_string()));
        assert!(session
            .history()
            .contains(&"[redacted environment assignment]".to_string()));
        assert_eq!(session.execute_line(". nested.rc").stdout, "nested\n");
        let with_arguments = session.execute_line("source args.rc alpha beta");
        assert_eq!(with_arguments.status, 0);
        assert_eq!(with_arguments.stdout, "args.rc alpha beta 2 alpha beta\n");
        let sourced_stdin = session.execute_line("echo piped | source stdin.rc");
        assert_eq!(sourced_stdin.status, 0);
        assert_eq!(sourced_stdin.stdout, "piped\n");
        let nested_arguments = session.execute_line("source outer.rc parent");
        assert_eq!(nested_arguments.status, 0);
        assert_eq!(
            nested_arguments.stdout,
            "args.rc inner  1 inner\nouter:parent\n"
        );
        let too_many_arguments = (0..65)
            .map(|index| format!("arg{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let rejected_arguments =
            session.execute_line(&format!("source nested.rc {too_many_arguments}"));
        assert_eq!(rejected_arguments.status, 2);
        assert!(rejected_arguments.stderr.contains("up to 64 arguments"));

        let missing = session.execute_line("source missing.rc");
        assert_eq!(missing.status, 1);
        assert!(missing.stderr.contains("source: "));
        assert_eq!(session.execute_line("source").status, 2);
        assert_eq!(session.execute_line("source one two").status, 1);

        std::fs::write(root.join("loop.rc"), b"source loop.rc\n").expect("loop file written");
        let recursive = session.execute_line("source loop.rc");
        assert_eq!(recursive.status, 2);
        assert!(recursive.stderr.contains(&format!(
            "source nesting exceeds the {MAX_SOURCE_DEPTH}-level limit"
        )));

        std::fs::write(root.join("oversized.rc"), vec![b'x'; MAX_SCRIPT_BYTES + 1])
            .expect("oversized source file written");
        let oversized = session.execute_line("source oversized.rc");
        assert_eq!(oversized.status, 2);
        assert!(oversized.stderr.contains("file exceeds the"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_sh_c_scripts_through_the_rust_planner() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("sh -c 'echo hello'").stdout, "hello\n");
        assert_eq!(
            session
                .execute_line("sh -c 'echo $0:$1:$2:$#:$@' runner first second")
                .stdout,
            "runner:first:second:2:first second\n"
        );
        assert_eq!(
            session.execute_line("echo piped | dash -c 'cat'").stdout,
            "piped\n"
        );
        assert_eq!(
            session.execute_line("sh -c 'echo one; echo two'").stdout,
            "one\ntwo\n"
        );
        assert_eq!(session.execute_line("sh").status, 2);
        assert_eq!(session.execute_line("sh -x 'echo not-run'").status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_for_loops_through_the_rust_planner() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let loop_output =
            session.execute_script("for item in one two three; do\n  echo \"$item\"\ndone\n");
        assert_eq!(loop_output.status, 0, "{loop_output:?}");
        assert_eq!(loop_output.stdout, "one\ntwo\nthree\n");
        assert_eq!(
            session.environment().get("item"),
            Some(&"three".to_string())
        );

        let inline = session.execute_script("for item in one two; do echo \"$item\"; done");
        assert_eq!(inline.status, 0, "{inline:?}");
        assert_eq!(inline.stdout, "one\ntwo\n");
        let quoted = session.execute_script("for item in one; do echo 'semi; done'; done");
        assert_eq!(quoted.status, 0, "{quoted:?}");
        assert_eq!(quoted.stdout, "semi; done\n");

        let nested = session.execute_script(
            "for outer in A B; do\nfor inner in 1 2; do\necho \"$outer$inner\"\ndone\ndone",
        );
        assert_eq!(nested.status, 0, "{nested:?}");
        assert_eq!(nested.stdout, "A1\nA2\nB1\nB2\n");

        let missing_done = session.execute_script("for value in one; do\necho $value");
        assert_eq!(missing_done.status, 2);
        assert!(missing_done.stderr.contains("missing `done`"));

        let too_many_values = (0..=MAX_FOR_VALUES)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let rejected_values = session.execute_script(&format!(
            "for value in {too_many_values}; do\necho $value\ndone"
        ));
        assert_eq!(rejected_values.status, 2);
        assert!(rejected_values.stderr.contains("value list exceeds"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_if_branches_through_the_rust_planner() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.execute_line("touch ready");

        let branch = session.execute_script(
            "if test -f missing; then\necho wrong\nelif test -f ready; then\necho ready\nelse\necho fallback\nfi",
        );
        assert_eq!(branch.status, 0, "{branch:?}");
        assert_eq!(branch.stdout, "ready\n");

        let nested = session
            .execute_script("if false\nthen\necho wrong\nelse\nif true; then\necho nested\nfi\nfi");
        assert_eq!(nested.status, 0, "{nested:?}");
        assert_eq!(nested.stdout, "nested\n");

        let no_branch = session.execute_script("if false; then\necho wrong\nfi");
        assert_eq!(no_branch.status, 0, "{no_branch:?}");
        assert!(no_branch.stdout.is_empty());

        let inline = session.execute_script("if true; then echo inline; fi");
        assert_eq!(inline.status, 0, "{inline:?}");
        assert_eq!(inline.stdout, "inline\n");
        let quoted = session.execute_script("if true; then echo 'semi; fi'; fi");
        assert_eq!(quoted.status, 0, "{quoted:?}");
        assert_eq!(quoted.stdout, "semi; fi\n");

        let missing_fi = session.execute_script("if true; then\necho incomplete");
        assert_eq!(missing_fi.status, 2);
        assert!(missing_fi.stderr.contains("missing `fi`"));

        let mut deep = String::new();
        for _ in 0..=MAX_SCRIPT_CONTROL_DEPTH {
            deep.push_str("if true; then\n");
        }
        for _ in 0..=MAX_SCRIPT_CONTROL_DEPTH {
            deep.push_str("fi\n");
        }
        let too_deep = session.execute_script(&deep);
        assert_eq!(too_deep.status, 2);
        assert!(too_deep.stderr.contains("control-flow nesting exceeds"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_while_and_until_loops_through_the_rust_planner() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let bounded = session.execute_script(
            "counter=0\nwhile test \"$counter\" -lt 3; do\necho \"$counter\"\nexport counter=3\ndone",
        );
        assert_eq!(bounded.status, 0, "{bounded:?}");
        assert_eq!(bounded.stdout, "0\n");

        let inline_while = session.execute_script(
            "counter=0\nwhile test \"$counter\" -lt 1; do echo \"$counter\"; export counter=1; done",
        );
        assert_eq!(inline_while.status, 0, "{inline_while:?}");
        assert_eq!(inline_while.stdout, "0\n");

        let inline_until = session.execute_script(
            "counter=1\nuntil test \"$counter\" -eq 0; do echo once; export counter=0; done",
        );
        assert_eq!(inline_until.status, 0, "{inline_until:?}");
        assert_eq!(inline_until.stdout, "once\n");

        let failed_body = session.execute_script(
            "counter=0\nwhile test \"$counter\" -lt 1; do\nexport counter=1\nfalse\ndone",
        );
        assert_eq!(failed_body.status, 1, "{failed_body:?}");

        let continued = session.execute_script(
            "for item in one two three; do\nif test \"$item\" = two; then\ncontinue\nfi\necho \"$item\"\ndone",
        );
        assert_eq!(continued.status, 0, "{continued:?}");
        assert_eq!(continued.stdout, "one\nthree\n");

        let stopped = session.execute_script(
            "for item in one two three; do\nif test \"$item\" = two; then\nbreak\nfi\necho \"$item\"\ndone",
        );
        assert_eq!(stopped.status, 0, "{stopped:?}");
        assert_eq!(stopped.stdout, "one\n");

        let outside = session.execute_script("break");
        assert_eq!(outside.status, 2);
        assert!(outside.stderr.contains("only valid inside a loop"));

        let isolated_shell =
            session.execute_script("for item in one; do\nsh -c 'break'\necho after\ndone");
        assert_eq!(isolated_shell.status, 0, "{isolated_shell:?}");
        assert_eq!(isolated_shell.stdout, "after\n");
        assert!(isolated_shell.stderr.contains("only valid inside a loop"));

        let until_output = session.execute_script(
            "counter=1\nuntil test \"$counter\" -eq 0; do\necho once\nexport counter=0\ndone\nif false; then\necho wrong\nfi",
        );
        assert_eq!(until_output.status, 0, "{until_output:?}");
        assert_eq!(until_output.stdout, "once\n");

        let infinite = session.execute_script("while true; do\ntrue\ndone");
        assert_eq!(infinite.status, 2);
        assert!(infinite.stderr.contains(&format!(
            "loop exceeds the {MAX_WHILE_ITERATIONS}-iteration limit"
        )));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_case_branches_through_the_rust_planner() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let selected = session.execute_script(
            "choice=beta\ncase \"$choice\" in\nalpha)\necho alpha\n;;\nbeta|gamma)\necho selected-$choice\n;;\n*)\necho fallback\n;;\nesac",
        );
        assert_eq!(selected.status, 0, "{selected:?}");
        assert_eq!(selected.stdout, "selected-beta\n");

        let wildcard = session.execute_script(
            "case report.txt in\n*.txt)\nif true; then\necho text\nfi\n;;\n*)\necho other\n;;\nesac",
        );
        assert_eq!(wildcard.status, 0, "{wildcard:?}");
        assert_eq!(wildcard.stdout, "text\n");

        let unmatched = session.execute_script("case image.png in\n*.txt)\necho wrong\n;;\nesac");
        assert_eq!(unmatched.status, 0, "{unmatched:?}");
        assert!(unmatched.stdout.is_empty());

        let missing_esac = session.execute_script("case value in\n*)\necho incomplete\n;;");
        assert_eq!(missing_esac.status, 2);
        assert!(missing_esac.stderr.contains("missing `esac`"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_script_functions_through_the_rust_planner() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let inline_definition = session.execute_line("inline() { echo inline-$1; }");
        assert_eq!(inline_definition.status, 0, "{inline_definition:?}");
        assert_eq!(
            session.execute_line("inline value").stdout,
            "inline-value\n"
        );

        let inline_script =
            session.execute_script("script_inline() { echo inline-$1; }\nscript_inline value");
        assert_eq!(inline_script.status, 0, "{inline_script:?}");
        assert_eq!(inline_script.stdout, "inline-value\n");

        let greeting =
            session.execute_script("greet() {\necho \"hello $1/$#/$@\"\n}\ngreet rune one\n");
        assert_eq!(greeting.status, 0, "{greeting:?}");
        assert_eq!(greeting.stdout, "hello rune/2/rune one\n");

        let shifted = session.execute_script(
            "shifted() {\necho \"$1:$#\"\nshift 2\necho \"$1:$#\"\n}\nshifted one two three",
        );
        assert_eq!(shifted.status, 0, "{shifted:?}");
        assert_eq!(shifted.stdout, "one:3\nthree:1\n");

        let shifted_shell = session.execute_script("sh -c 'shift; echo $1:$#' runner one two");
        assert_eq!(shifted_shell.status, 0, "{shifted_shell:?}");
        assert_eq!(shifted_shell.stdout, "two:1\n");

        let outside_shift = session.execute_script("shift");
        assert_eq!(outside_shift.status, 2);
        assert!(outside_shift
            .stderr
            .contains("shift: only valid inside a script or function"));

        let invalid_shift =
            session.execute_script("invalid_shift() {\nshift nope\n}\ninvalid_shift");
        assert_eq!(invalid_shift.status, 2);
        assert!(invalid_shift
            .stderr
            .contains("shift: count must be a non-negative integer"));

        let excessive_shift =
            session.execute_script("excessive_shift() {\nshift 2\n}\nexcessive_shift one");
        assert_eq!(excessive_shift.status, 2);
        assert!(excessive_shift
            .stderr
            .contains("shift: count 2 exceeds 1 positional arguments"));

        let nested = session.execute_script(
            "outer() {\ninner() {\necho inner\n}\nif true; then\ninner\nfi\n}\nouter",
        );
        assert_eq!(nested.status, 0, "{nested:?}");
        assert_eq!(nested.stdout, "inner\n");

        let conditional = session
            .execute_script("if true; then\nconditional() {\necho conditional\n}\nfi\nconditional");
        assert_eq!(conditional.status, 0, "{conditional:?}");
        assert_eq!(conditional.stdout, "conditional\n");

        let loop_defined = session.execute_script(
            "for value in one; do\nloop_defined() {\necho loop\n}\ndone\nloop_defined",
        );
        assert_eq!(loop_defined.status, 0, "{loop_defined:?}");
        assert_eq!(loop_defined.stdout, "loop\n");

        let case_defined = session.execute_script(
            "case yes in\nyes)\ncase_defined() {\necho case\n}\n;;\nesac\ncase_defined",
        );
        assert_eq!(case_defined.status, 0, "{case_defined:?}");
        assert_eq!(case_defined.stdout, "case\n");

        let stateful = session.execute_script(
            "set_name() {\nexport FUNCTION_NAME=$1\n}\nset_name rune\necho \"$FUNCTION_NAME\"",
        );
        assert_eq!(stateful.status, 0, "{stateful:?}");
        assert_eq!(stateful.stdout, "rune\n");

        let local_scope = session.execute_script(
            "export SCOPE=outer\nscoped() {\nlocal SCOPE=inner NEW_VALUE\necho \"$SCOPE:$NEW_VALUE\"\n}\nscoped\necho \"$SCOPE:$NEW_VALUE\"",
        );
        assert_eq!(local_scope.status, 0, "{local_scope:?}");
        assert_eq!(local_scope.stdout, "inner:\nouter:\n");

        let nested_local_scope = session.execute_script(
            "inner_scope() {\nlocal SCOPE=inner\necho \"$SCOPE\"\n}\nouter_scope() {\nlocal SCOPE=outer\ninner_scope\necho \"$SCOPE\"\n}\nouter_scope\necho \"$SCOPE\"",
        );
        assert_eq!(nested_local_scope.status, 0, "{nested_local_scope:?}");
        assert_eq!(nested_local_scope.stdout, "inner\nouter\nouter\n");

        let function_parameters = session.execute_script(
            "set_parameters() {\nset -- inner value\necho \"$0:$#:$1:$2\"\n}\nset_parameters outer",
        );
        assert_eq!(function_parameters.status, 0, "{function_parameters:?}");
        assert_eq!(function_parameters.stdout, "set_parameters:2:inner:value\n");

        let reset_parameters =
            session.execute_script("set -- first second\necho \"$#:$1:$2\"\nshift\necho \"$#:$1\"");
        assert_eq!(reset_parameters.status, 0, "{reset_parameters:?}");
        assert_eq!(reset_parameters.stdout, "2:first:second\n1:second\n");

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn bounds_script_function_control_and_namespace_limits() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let outside_local = session.execute_script("local SCOPE=value");
        assert_eq!(outside_local.status, 2);
        assert!(outside_local
            .stderr
            .contains("local: only valid inside a function"));

        let too_many_locals = (0..=MAX_LOCAL_VARIABLES)
            .map(|index| format!("LOCAL_{index}=value"))
            .collect::<Vec<_>>()
            .join(" ");
        let local_limit = session.execute_script(&format!(
            "too_many_locals() {{\nlocal {too_many_locals}\n}}\ntoo_many_locals"
        ));
        assert_eq!(local_limit.status, 2);
        assert!(local_limit.stderr.contains("up to 64 variables"));

        let redefined = session.execute_script("greet() {\necho replacement\n}\ngreet");
        assert_eq!(redefined.status, 0, "{redefined:?}");
        assert_eq!(redefined.stdout, "replacement\n");

        let early_return =
            session.execute_script("finish() {\necho before\nreturn 7\necho after\n}\nfinish");
        assert_eq!(early_return.status, 7, "{early_return:?}");
        assert_eq!(early_return.stdout, "before\n");

        let implicit_return =
            session.execute_script("use_last_status() {\nfalse\nreturn\n}\nuse_last_status");
        assert_eq!(implicit_return.status, 1, "{implicit_return:?}");
        assert!(implicit_return.stdout.is_empty());

        let loop_return = session.execute_script(
            "return_from_loop() {\nfor item in one two; do\necho $item\nreturn 9\ndone\necho after\n}\nreturn_from_loop",
        );
        assert_eq!(loop_return.status, 9, "{loop_return:?}");
        assert_eq!(loop_return.stdout, "one\n");

        let outside_return = session.execute_script("return 4");
        assert_eq!(outside_return.status, 2);
        assert!(outside_return
            .stderr
            .contains("return: only valid inside a function"));

        let invalid_return =
            session.execute_script("invalid_status() {\nreturn 256\n}\ninvalid_status");
        assert_eq!(invalid_return.status, 2);
        assert!(invalid_return
            .stderr
            .contains("status must be an integer from 0 through 255"));

        let invalid_set = session.execute_script("set first");
        assert_eq!(invalid_set.status, 2);
        assert!(invalid_set.stderr.contains("usage: set -- [ARG ...]"));

        let too_many_set_arguments = (0..=MAX_SOURCE_ARGUMENTS)
            .map(|index| format!("arg{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let rejected_set = session.execute_script(&format!("set -- {too_many_set_arguments}"));
        assert_eq!(rejected_set.status, 2);
        assert!(rejected_set
            .stderr
            .contains("up to 64 positional arguments"));

        let isolated_shell = session.execute_script("sh -c 'greet'");
        assert_eq!(isolated_shell.status, 127, "{isolated_shell:?}");
        assert!(isolated_shell.stderr.contains("greet: command not found"));
        let isolated_substitution = session.execute_script("echo \"$(greet)\"");
        assert_eq!(isolated_substitution.status, 0, "{isolated_substitution:?}");
        assert_eq!(isolated_substitution.stdout, "\n");
        assert!(isolated_substitution
            .stderr
            .contains("greet: command not found"));

        let too_many_arguments = (0..=MAX_FUNCTION_ARGUMENTS)
            .map(|index| format!("arg{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let rejected_arguments = session.execute_script(&format!("greet {too_many_arguments}"));
        assert_eq!(rejected_arguments.status, 2);
        assert!(rejected_arguments.stderr.contains("up to 64 arguments"));

        let recursive = session.execute_script("recurse() {\nrecurse\n}\nrecurse");
        assert_eq!(recursive.status, 2, "{recursive:?}");
        assert!(recursive.stderr.contains(&format!(
            "function recursion exceeds the {MAX_FUNCTION_DEPTH}-level limit"
        )));

        let too_long_name = format!("{}() {{\ntrue\n}}", "a".repeat(MAX_FUNCTION_NAME_BYTES + 1));
        let rejected_name = session.execute_script(&too_long_name);
        assert_eq!(rejected_name.status, 2);
        assert!(rejected_name.stderr.contains("function name exceeds"));

        let missing_brace = session.execute_script("broken() {\necho incomplete");
        assert_eq!(missing_brace.status, 2);
        assert!(missing_brace.stderr.contains("function is missing `}`"));

        let mut too_many_functions = String::new();
        for index in 0..=MAX_FUNCTIONS {
            let _ = writeln!(
                too_many_functions,
                "function_{index}() {{
true
}}",
            );
        }
        let rejected_functions = session.execute_script(&too_many_functions);
        assert_eq!(rejected_functions.status, 2);
        assert!(rejected_functions.stderr.contains("definition limit"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn routes_toolchain_commands_through_an_explicit_provider_and_vfs() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session
            .write_file("hello.c", b"int main(void) { return 0; }")
            .expect("source file written");

        let unavailable = session.execute_line("cc hello.c");
        assert_eq!(unavailable.status, 126);
        assert!(unavailable
            .stderr
            .contains("toolchain provider is unavailable"));
        assert_eq!(session.execute_line("command -v cc").stdout, "cc\n");

        session.set_toolchain_provider(Box::new(RecordingToolchainProvider {
            kind: ToolchainKind::C,
        }));
        let compiled = session.execute_line("clang hello.c --target=wasm32-wasi");
        assert_eq!(compiled.status, 0);
        assert_eq!(compiled.stdout, "compiled hello.c\n");
        assert_eq!(
            session.execute_line("cat hello.wasm").stdout,
            "fake-compiled-artifact"
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn observes_cooperative_cancellation_at_command_boundaries() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session.cancel();
        let cancelled = session.execute_line("echo should-not-run");
        assert_eq!(cancelled.status, CANCELLED_STATUS);
        assert_eq!(cancelled.stdout, "");
        assert_eq!(cancelled.stderr, "rune: command cancelled\n");
        assert!(session.history().is_empty());

        assert_eq!(session.execute_line("echo resumed").stdout, "resumed\n");
        session.cancel();
        let script = session.execute_script("echo first\necho second");
        assert_eq!(script.status, CANCELLED_STATUS);
        assert_eq!(script.stdout, "");
        assert_eq!(script.stderr, "rune: command cancelled\n");
        assert!(!session.history().contains(&"echo first".to_string()));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn command_context_consumes_cancellation_for_long_operations() {
        let root = test_root();
        let mut filesystem = SandboxedFileSystem::new(&root).expect("sandbox filesystem created");
        let mut environment = std::collections::BTreeMap::new();
        let mut aliases = std::collections::BTreeMap::new();
        let mut bookmarks = std::collections::BTreeMap::new();
        let mut directory_usage = std::collections::BTreeMap::new();
        let mut config = super::TerminalConfig::default();
        let mut history = Vec::new();
        let registry = super::CommandRegistry::default();
        let runtime = rune_wasm::WasmRunner::default();
        let toolchains = super::ToolchainProviders::default();
        let network = super::DisabledNetworkProvider;
        let clipboard = super::DisabledClipboardProvider;
        let opener = super::DisabledOpenProvider;
        let args = Vec::new();
        let cancellation = std::sync::atomic::AtomicBool::new(true);
        let cancelled = {
            let context = super::CommandContext {
                args: &args,
                stdin: "",
                fs: &mut filesystem,
                env: &mut environment,
                aliases: &mut aliases,
                bookmarks: &mut bookmarks,
                directory_usage: &mut directory_usage,
                config: &mut config,
                history: &mut history,
                command_definitions: registry.definitions(),
                runtime: &runtime,
                python_runtime: &runtime,
                lua_runtime: &runtime,
                javascript_runtime: &runtime,
                toolchains: &toolchains,
                network: &network,
                clipboard: &clipboard,
                opener: &opener,
                cancellation: &cancellation,
            };
            context
                .take_cancellation()
                .expect("cancellation should be observed")
        };
        assert_eq!(cancelled.status, CANCELLED_STATUS);
        assert_eq!(cancelled.stderr, "rune: command cancelled\n");
        assert!(!cancellation.load(Ordering::Acquire));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn sleep_observes_cancellation_while_waiting() {
        let root = test_root();
        let mut filesystem = SandboxedFileSystem::new(&root).expect("sandbox filesystem created");
        let mut environment = std::collections::BTreeMap::new();
        let mut aliases = std::collections::BTreeMap::new();
        let mut bookmarks = std::collections::BTreeMap::new();
        let mut directory_usage = std::collections::BTreeMap::new();
        let mut config = super::TerminalConfig::default();
        let mut history = Vec::new();
        let registry = super::CommandRegistry::default();
        let runtime = rune_wasm::WasmRunner::default();
        let toolchains = super::ToolchainProviders::default();
        let network = super::DisabledNetworkProvider;
        let clipboard = super::DisabledClipboardProvider;
        let opener = super::DisabledOpenProvider;
        let args = vec!["1".to_string()];
        let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let trigger = std::sync::Arc::clone(&cancellation);
        let trigger_thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            trigger.store(true, std::sync::atomic::Ordering::Release);
        });
        let mut context = super::CommandContext {
            args: &args,
            stdin: "",
            fs: &mut filesystem,
            env: &mut environment,
            aliases: &mut aliases,
            bookmarks: &mut bookmarks,
            directory_usage: &mut directory_usage,
            config: &mut config,
            history: &mut history,
            command_definitions: registry.definitions(),
            runtime: &runtime,
            python_runtime: &runtime,
            lua_runtime: &runtime,
            javascript_runtime: &runtime,
            toolchains: &toolchains,
            network: &network,
            clipboard: &clipboard,
            opener: &opener,
            cancellation: cancellation.as_ref(),
        };
        let cancelled = super::commands::shell::sleep(&mut context);
        trigger_thread.join().expect("cancellation trigger joined");
        assert_eq!(cancelled.status, CANCELLED_STATUS);
        assert_eq!(cancelled.stderr, "rune: command cancelled\n");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn rejects_an_oversized_command_line_before_recording_or_parsing() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let input = format!("echo {}", "x".repeat(MAX_COMMAND_INPUT_BYTES));
        let output = session.execute_line(&input);
        assert_eq!(output.status, 2);
        assert!(output.stderr.contains("command line exceeds"));
        assert!(session.history().is_empty());
        assert_eq!(session.last_status(), 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn bounds_total_history_storage_even_when_record_limit_is_large() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session
                .execute_line("config set history-limit 10000")
                .status,
            0
        );

        let long_command = format!("true {}", "x".repeat(MAX_COMMAND_INPUT_BYTES - 5));
        let repetitions = MAX_HISTORY_BYTES / long_command.len() + 2;
        for _ in 0..repetitions {
            assert_eq!(session.execute_line(&long_command).status, 2);
        }

        let stored_bytes: usize = session
            .history()
            .iter()
            .map(|command| "history=".len() + command.len() + 1)
            .sum();
        assert!(stored_bytes <= MAX_HISTORY_BYTES);
        assert!(session.history().len() < repetitions + 1);
        session.persist().expect("bounded history persisted");
        assert!(
            std::fs::metadata(root.join(".rune/session.state"))
                .expect("session state exists")
                .len()
                <= u64::try_from(MAX_HISTORY_BYTES + 64).expect("history size fits in u64")
        );

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn bounds_session_bookmarks_by_count_and_name_size() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        for index in 0..MAX_BOOKMARKS {
            assert_eq!(
                session
                    .execute_line(&format!("bookmark mark{index}"))
                    .status,
                0
            );
        }
        let too_many = session.execute_line("bookmark overflow");
        assert_eq!(too_many.status, 1);
        assert!(too_many.stderr.contains("maximum of 256 bookmarks"));
        assert_eq!(session.bookmarks().len(), MAX_BOOKMARKS);

        let long_name = format!("bookmark {}", "x".repeat(MAX_BOOKMARK_NAME_CHARS + 1));
        assert_eq!(session.execute_line(&long_name).status, 2);
        assert_eq!(session.bookmarks().len(), MAX_BOOKMARKS);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn keeps_command_registry_names_unique() {
        let mut names = super::CommandRegistry::default()
            .definitions()
            .iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        names.sort_unstable();

        assert!(
            names.windows(2).all(|pair| pair[0] != pair[1]),
            "duplicate command definition: {names:?}"
        );
    }

    #[test]
    fn supports_short_bookmark_aliases_through_the_rust_registry() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        assert_eq!(session.execute_line("mkdir src && cd src").status, 0);
        assert_eq!(session.execute_line("s source").status, 0);
        assert_eq!(session.execute_line("cd ..").status, 0);
        assert_eq!(session.execute_line("g source").status, 0);
        assert_eq!(session.current_directory(), "~/src");
        assert_eq!(session.execute_line("l").stdout, "source -> ~/src\n");
        assert_eq!(session.execute_line("p").stdout, "source -> ~/src\n");
        assert_eq!(session.execute_line("r source renamed").status, 0);
        assert_eq!(session.execute_line("d renamed").status, 0);
        assert!(session.execute_line("l").stdout.is_empty());

        let invalid = session.execute_line("d missing");
        assert_eq!(invalid.status, 1);
        assert!(invalid
            .stderr
            .contains("deletemark: missing: bookmark not found"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn jumps_to_the_most_frequently_visited_matching_directory() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session
                .execute_line("mkdir -p projects/alpha archives/alpha")
                .status,
            0
        );
        assert_eq!(session.execute_line("cd projects && cd alpha").status, 0);
        assert_eq!(session.execute_line("cd ../..").status, 0);
        assert_eq!(session.execute_line("cd archives && cd alpha").status, 0);
        assert_eq!(session.execute_line("cd ../..").status, 0);
        assert_eq!(session.execute_line("cd projects && cd alpha").status, 0);
        assert_eq!(session.execute_line("cd ../..").status, 0);

        assert_eq!(session.execute_line("mkdir -p stale/alpha").status, 0);
        for _ in 0..4 {
            assert_eq!(session.execute_line("cd stale && cd alpha").status, 0);
            assert_eq!(session.execute_line("cd ../..").status, 0);
        }
        assert_eq!(session.execute_line("rm -r stale").status, 0);

        let jumped = session.execute_line("z alpha");
        assert_eq!(jumped.status, 0, "{jumped:?}");
        assert_eq!(session.current_directory(), "~/projects/alpha");
        assert_eq!(session.directory_usage.get("~/projects/alpha"), Some(&3));

        let no_match = session.execute_line("z missing");
        assert_eq!(no_match.status, 1);
        assert_eq!(no_match.stderr, "z: no directory matches missing\n");
        let invalid = session.execute_line("z");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("usage: z KEYWORD ..."));

        session.persist().expect("directory usage persisted");
        let restored = Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert_eq!(restored.directory_usage.get("~/projects/alpha"), Some(&3));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn z_falls_back_to_matching_direct_children_without_usage_history() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("mkdir archive").status, 0);
        let output = session.execute_line("z chi");
        assert_eq!(output.status, 0, "{output:?}");
        assert_eq!(session.current_directory(), "~/archive");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn formats_bounded_printf_arguments() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session
                .execute_line(r"printf 'name=%s count=%d\n' Rune 3")
                .stdout,
            "name=Rune count=3\n"
        );
        assert_eq!(session.execute_line("printf '100%%'").stdout, "100%");
        assert_eq!(
            session.execute_line(r"printf '\033[?25l'").stdout,
            "\u{1b}[?25l"
        );
        assert_eq!(
            session.execute_line(r"printf '\x1b[?25h'").stdout,
            "\u{1b}[?25h"
        );
        let invalid = session.execute_line("printf '%d' nope");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("integer argument is invalid"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn prints_bounded_integer_sequences() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("seq 3").stdout, "0\n1\n2\n3\n");
        assert_eq!(session.execute_line("seq 2 2 6").stdout, "2\n4\n6\n");
        assert_eq!(
            session.execute_line("seq 3 -1 -1").stdout,
            "3\n2\n1\n0\n-1\n"
        );
        assert_eq!(session.execute_line("seq 5 1 3").stdout, "");
        let zero = session.execute_line("seq 1 0 3");
        assert_eq!(zero.status, 1);
        assert!(zero.stderr.contains("must not be zero"));
        let too_large = session.execute_line("seq 1 100001");
        assert_eq!(too_large.status, 1);
        assert!(too_large.stderr.contains("exceeds 100000"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn manages_virtual_bookmarks_and_restores_them() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session.execute_line("mkdir project && cd project").status,
            0
        );
        assert_eq!(session.execute_line("bookmark work").status, 0);
        assert_eq!(session.execute_line("bookmark bad/name").status, 2);
        assert_eq!(session.execute_line("cd ~").status, 0);
        assert_eq!(session.execute_line("cd ~work").status, 0);
        assert_eq!(session.current_directory(), "~/project");
        assert_eq!(
            session.execute_line("showmarks").stdout,
            "work -> ~/project\n"
        );
        assert_eq!(session.execute_line("renamemark work source").status, 0);
        assert_eq!(session.execute_line("bookmark other").status, 0);
        assert_eq!(session.execute_line("renamemark source other").status, 1);
        assert_eq!(session.execute_line("cd ~source").status, 0);
        assert_eq!(session.execute_line("deletemark source").status, 0);
        assert_eq!(session.execute_line("jump source").status, 1);

        assert_eq!(session.execute_line("bookmark persisted").status, 0);
        session.persist().expect("bookmarks persisted");
        let mut restored =
            Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert_eq!(restored.bookmarks()["persisted"], "~/project");
        assert_eq!(restored.execute_line("cd ~persisted").status, 0);
        assert_eq!(restored.current_directory(), "~/project");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn walks_bounded_filesystem_with_find_filters_and_depth() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session
                .execute_line("mkdir -p project/src project/docs")
                .status,
            0
        );
        assert_eq!(
            session
                .execute_line("touch project/src/main.rs project/README.md")
                .status,
            0
        );
        assert_eq!(
            session.execute_line("find project -maxdepth 1").stdout,
            "project\nproject/README.md\nproject/docs\nproject/src\n"
        );
        assert_eq!(
            session.execute_line("find project -name '*.rs'").stdout,
            "project/src/main.rs\n"
        );
        assert_eq!(
            session
                .execute_line("find project -type f -name '*.rs'")
                .stdout,
            "project/src/main.rs\n"
        );
        assert_eq!(
            session
                .execute_line("find project -type d -mindepth 1 -maxdepth 1")
                .stdout,
            "project/docs\nproject/src\n"
        );
        assert_eq!(
            session
                .execute_line("ln -s src/main.rs project/link.rs")
                .status,
            0
        );
        assert_eq!(
            session.execute_line("find project -type l").stdout,
            "project/link.rs\n"
        );
        let missing = session.execute_line("find missing");
        assert_eq!(missing.status, 1);
        assert!(missing.stderr.contains("no such file or directory"));
        let invalid = session.execute_line("find project -maxdepth many");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("non-negative number"));
        assert_eq!(session.execute_line("find project -type x").status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn keeps_stdout_stderr_and_status_separate() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let output = session.execute_line("missing-command");
        assert!(output.stdout.is_empty());
        assert!(output.stderr.contains("command not found"));
        assert_eq!(output.status, 127);
        assert_eq!(session.execute_line("echo $?").stdout, "127\n");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn bounds_large_terminal_output_without_changing_command_status() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        std::fs::write(
            root.join("large.txt"),
            vec![b'x'; MAX_OUTPUT_BYTES + OUTPUT_TRUNCATION_MARKER.len() + 128],
        )
        .expect("large file written");
        let output = session.execute_line("cat large.txt");
        assert_eq!(output.status, 0);
        assert!(output.stdout.len() <= MAX_OUTPUT_BYTES);
        assert!(output.stdout.ends_with(OUTPUT_TRUNCATION_MARKER));
        assert_eq!(output.stderr, OUTPUT_TRUNCATION_MARKER);
        let piped = session.execute_line("cat large.txt | cat");
        assert_eq!(piped.status, 0);
        assert!(piped.stdout.len() <= MAX_OUTPUT_BYTES);
        assert!(piped.stdout.ends_with(OUTPUT_TRUNCATION_MARKER));
        assert_eq!(piped.stderr, OUTPUT_TRUNCATION_MARKER);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_wasi_module_loaded_through_the_virtual_filesystem() {
        let root = test_root();
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                  (memory (export "memory") 1)
                  (data (i32.const 8) "hello from rune wasm\n")
                  (data (i32.const 0) "\08\00\00\00\15\00\00\00")
                  (func (export "_start")
                    (i32.const 1)
                    (i32.const 0)
                    (i32.const 1)
                    (i32.const 24)
                    (call $fd_write)
                    (drop)))
            "#,
        )
        .expect("valid WAT");
        std::fs::write(root.join("hello.wasm"), wasm).expect("module written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let output = session.execute_line("wasm hello.wasm demo-arg");
        assert_eq!(output.status, 0);
        assert_eq!(output.stdout, "hello from rune wasm\n");
        assert!(output.stderr.is_empty());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn passes_the_sandbox_root_to_the_wasm_builtin_as_a_preopen() {
        let root = test_root();
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "fd_prestat_get"
                    (func $fd_prestat_get (param i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                  (memory (export "memory") 1)
                  (data (i32.const 0) "\40\00\00\00\12\00\00\00")
                  (data (i32.const 32) "\64\00\00\00\0c\00\00\00")
                  (data (i32.const 64) "preopen available\n")
                  (data (i32.const 100) "preopen absent\n")
                  (func (export "_start")
                    (i32.const 3)
                    (i32.const 48)
                    (call $fd_prestat_get)
                    (if
                      (then
                        (i32.const 1)
                        (i32.const 32)
                        (i32.const 1)
                        (i32.const 24)
                        (call $fd_write)
                        (drop))
                      (else
                        (i32.const 1)
                        (i32.const 0)
                        (i32.const 1)
                        (i32.const 24)
                        (call $fd_write)
                        (drop)))))
            "#,
        )
        .expect("valid preopen WAT");
        std::fs::write(root.join("preopen.wasm"), wasm).expect("module written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let output = session.execute_line("wasm preopen.wasm");
        assert_eq!(output.status, 0);
        assert_eq!(output.stdout, "preopen available\n");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn passes_named_layout_mounts_to_the_wasm_builtin() {
        let container = test_root();
        let home = container.join("Documents");
        let library = container.join("Library");
        let temporary = container.join("tmp");
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "fd_prestat_get"
                    (func $fd_prestat_get (param i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                  (memory (export "memory") 1)
                  (data (i32.const 0) "\40\00\00\00\08\00\00\00")
                  (data (i32.const 32) "\64\00\00\00\04\00\00\00")
                  (data (i32.const 64) "library\n")
                  (data (i32.const 100) "tmp\n")
                  (func (export "_start")
                    (i32.const 4)
                    (i32.const 48)
                    (call $fd_prestat_get)
                    (i32.eqz)
                    (if
                      (then
                        (i32.const 1)
                        (i32.const 0)
                        (i32.const 1)
                        (i32.const 24)
                        (call $fd_write)
                        (drop)))
                    (i32.const 5)
                    (i32.const 48)
                    (call $fd_prestat_get)
                    (i32.eqz)
                    (if
                      (then
                        (i32.const 1)
                        (i32.const 32)
                        (i32.const 1)
                        (i32.const 24)
                        (call $fd_write)
                        (drop)))))
            "#,
        )
        .expect("valid layout preopen WAT");
        let filesystem = SandboxedFileSystem::new_with_layout(&home, &library, &temporary)
            .expect("layout created");
        std::fs::write(home.join("mounts.wasm"), wasm).expect("module written");
        let mut session = Session::new(filesystem);
        let output = session.execute_line("wasm mounts.wasm");
        assert_eq!(output.status, 0, "{output:?}");
        assert_eq!(output.stdout, "library\ntmp\n", "{output:?}");
        assert!(output.stderr.is_empty());
        std::fs::remove_dir_all(container).expect("test container removed");
    }

    #[test]
    fn inspects_and_verifies_a_local_package_manifest() {
        let root = test_root();
        std::fs::create_dir_all(root.join("bundle/bin")).expect("package directories created");
        std::fs::write(root.join("bundle/bin/hello.wasm"), b"hello").expect("artifact written");
        std::fs::write(
            root.join("bundle/manifest.json"),
            br#"{
                "schema_version": 1,
                "name": "hello-rune",
                "version": "0.1.0",
                "description": "A local package",
                "files": [{
                    "path": "bin/hello.wasm",
                    "sha256": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
                }],
                "commands": [{"name": "hello", "entry": "bin/hello.wasm"}]
            }"#,
        )
        .expect("manifest written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let info = session.execute_line("pkg info bundle/manifest.json");
        assert_eq!(info.status, 0);
        assert!(info.stdout.contains("hello-rune 0.1.0"));
        assert!(info.stdout.contains("command: hello -> bin/hello.wasm"));
        let verified = session.execute_line("pkg verify bundle/manifest.json");
        assert_eq!(verified.status, 0);
        assert_eq!(verified.stdout, "hello-rune@0.1.0: verified 1 files\n");

        std::fs::write(root.join("bundle/bin/hello.wasm"), b"tampered").expect("artifact modified");
        let mismatch = session.execute_line("pkg verify bundle/manifest.json");
        assert_eq!(mismatch.status, 1);
        assert!(mismatch.stderr.contains("integrity mismatch"));
        let empty_search = session.execute_line("pkg search hello");
        assert_eq!(empty_search.status, 0);
        assert!(empty_search.stdout.is_empty());
        let oversized_query = format!("pkg search {}", "x".repeat(65));
        assert_eq!(session.execute_line(&oversized_query).status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    fn package_wasm_probe() -> Vec<u8> {
        wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "fd_prestat_get"
                    (func $fd_prestat_get (param i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "proc_exit"
                    (func $proc_exit (param i32)))
                  (memory (export "memory") 1)
                  (func (export "_start")
                    (i32.const 3)
                    (i32.const 0)
                    (call $fd_prestat_get)
                    (i32.eqz)
                    (if
                      (then
                        (i32.const 9)
                        (call $proc_exit))
                      (else
                        (i32.const 7)
                        (call $proc_exit)))))
            "#,
        )
        .expect("valid package module")
    }

    #[test]
    fn installs_lists_runs_and_removes_a_verified_wasm_package() {
        let root = test_root();
        let package_root = root.join("bundle/bin");
        std::fs::create_dir_all(&package_root).expect("package directories created");
        let wasm = package_wasm_probe();
        let digest = rune_package::sha256_hex(&wasm);
        std::fs::write(package_root.join("hello.wasm"), &wasm).expect("module written");
        let manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-wasm",
                "version": "0.1.0",
                "description": "A local WASM package",
                "files": [{{"path": "bin/hello.wasm", "sha256": "{digest}"}}],
                "commands": [{{"name": "local-hello", "entry": "bin/hello.wasm"}}]
            }}"#
        );
        std::fs::write(root.join("bundle/manifest.json"), &manifest).expect("manifest written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let installed = session.execute_line("pkg install bundle/manifest.json");
        assert_eq!(installed.status, 0);
        assert_eq!(installed.stdout, "installed local-wasm@0.1.0\n");
        assert_eq!(session.completion_candidates("local-"), vec!["local-hello"]);
        assert_eq!(
            session.execute_line("pkg list").stdout,
            "local-wasm@0.1.0\n"
        );
        assert_eq!(
            session.execute_line("pkg search wasm").stdout,
            "local-wasm@0.1.0\tA local WASM package\n"
        );
        let installed_info = session.execute_line("pkg info local-wasm");
        assert_eq!(installed_info.status, 0);
        assert!(installed_info.stdout.contains("local-wasm 0.1.0"));
        assert!(installed_info.stdout.contains("permissions: none"));
        let versioned_info = session.execute_line("pkg info local-wasm 0.1.0");
        assert_eq!(versioned_info.status, 0);
        assert!(versioned_info
            .stdout
            .contains("command: local-hello -> bin/hello.wasm"));
        let second_version = root.join(".rune/packages/local-wasm/0.2.0");
        std::fs::create_dir_all(&second_version).expect("second package version directory created");
        std::fs::write(second_version.join("manifest.json"), &manifest)
            .expect("second package manifest written");
        let ambiguous_info = session.execute_line("pkg info local-wasm");
        assert_eq!(ambiguous_info.status, 2);
        assert!(ambiguous_info.stderr.contains("multiple versions"));
        assert_eq!(
            session.execute_line("pkg remove local-wasm 0.2.0").status,
            0
        );
        assert_eq!(
            session.execute_line("pkg search LOCAL-HELLO").stdout,
            "local-wasm@0.1.0\tA local WASM package\n"
        );
        assert!(session.execute_line("pkg search missing").stdout.is_empty());
        assert_eq!(session.execute_line("pkg search").status, 2);
        assert_eq!(
            session.execute_line("which local-hello").stdout,
            "local-hello: package local-wasm@0.1.0\n"
        );
        let command_output = session.execute_line("local-hello argument");
        assert_eq!(command_output.status, 7, "{command_output:?}");
        std::fs::write(
            root.join(".rune/packages/local-wasm/0.1.0/bin/hello.wasm"),
            b"tampered",
        )
        .expect("installed module modified");
        let tampered = session.execute_line("local-hello");
        assert_eq!(tampered.status, 126);
        assert!(tampered.stderr.contains("integrity failure"));
        assert_eq!(
            session
                .execute_line("pkg install bundle/manifest.json")
                .status,
            1
        );
        assert_eq!(
            session.execute_line("pkg remove local-wasm 0.1.0").stdout,
            "removed local-wasm@0.1.0\n"
        );
        assert!(session.execute_line("pkg list").stdout.is_empty());
        let removed_lookup = session.execute_line("which local-hello");
        assert_eq!(removed_lookup.status, 1);
        assert_eq!(session.execute_line("local-hello").status, 127);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn updates_an_installed_package_from_a_verified_local_manifest() {
        let root = test_root();
        let package_root = root.join("bundle/bin");
        std::fs::create_dir_all(&package_root).expect("package directories created");
        let wasm = package_wasm_probe();
        let digest = rune_package::sha256_hex(&wasm);
        std::fs::write(package_root.join("hello.wasm"), &wasm).expect("module written");

        let initial_manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-update",
                "version": "0.1.0",
                "description": "Initial package",
                "files": [{{"path": "bin/hello.wasm", "sha256": "{digest}"}}],
                "commands": [{{"name": "local-update-command", "entry": "bin/hello.wasm"}}]
            }}"#
        );
        let manifest_path = root.join("bundle/manifest.json");
        std::fs::write(&manifest_path, &initial_manifest).expect("initial manifest written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let installed = session.execute_line("pkg install bundle/manifest.json");
        assert_eq!(installed.status, 0);
        assert_eq!(installed.stdout, "installed local-update@0.1.0\n");
        assert_eq!(session.execute_line("local-update-command").status, 7);

        let invalid_update_manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-update",
                "version": "0.2.0",
                "description": "Rejected update",
                "files": [{{"path": "bin/hello.wasm", "sha256": "{}"}}],
                "commands": [{{"name": "local-update-command", "entry": "bin/hello.wasm"}}]
            }}"#,
            "0".repeat(64)
        );
        std::fs::write(&manifest_path, invalid_update_manifest).expect("invalid update written");
        let rejected = session.execute_line("pkg update bundle/manifest.json");
        assert_eq!(rejected.status, 1);
        assert!(rejected.stderr.contains("integrity mismatch"));
        assert_eq!(
            session.execute_line("pkg list").stdout,
            "local-update@0.1.0\n"
        );
        assert_eq!(session.execute_line("local-update-command").status, 7);

        let valid_update_manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-update",
                "version": "0.2.0",
                "description": "Updated package",
                "files": [{{"path": "bin/hello.wasm", "sha256": "{digest}"}}],
                "commands": [{{"name": "local-update-command", "entry": "bin/hello.wasm"}}]
            }}"#
        );
        std::fs::write(&manifest_path, valid_update_manifest).expect("valid update written");
        let updated = session.execute_line("pkg update bundle/manifest.json");
        assert_eq!(updated.status, 0);
        assert_eq!(updated.stdout, "updated local-update from 0.1.0 to 0.2.0\n");
        assert_eq!(
            session.execute_line("pkg list").stdout,
            "local-update@0.2.0\n"
        );
        assert!(session
            .execute_line("pkg info local-update")
            .stdout
            .contains("Updated package"));
        assert_eq!(
            session.execute_line("pkg info local-update 0.1.0").status,
            1
        );
        assert_eq!(session.execute_line("local-update-command").status, 7);
        assert_eq!(
            session
                .execute_line("pkg update bundle/manifest.json")
                .status,
            1
        );

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn rejects_a_package_update_when_the_installed_manifest_name_is_tampered() {
        let root = test_root();
        let package_root = root.join("bundle/bin");
        std::fs::create_dir_all(&package_root).expect("package directories created");
        let wasm = package_wasm_probe();
        let digest = rune_package::sha256_hex(&wasm);
        std::fs::write(package_root.join("hello.wasm"), &wasm).expect("module written");
        let manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-update",
                "version": "0.1.0",
                "description": "Initial package",
                "files": [{{"path": "bin/hello.wasm", "sha256": "{digest}"}}],
                "commands": [{{"name": "local-update-command", "entry": "bin/hello.wasm"}}]
            }}"#
        );
        let manifest_path = root.join("bundle/manifest.json");
        std::fs::write(&manifest_path, &manifest).expect("manifest written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session
                .execute_line("pkg install bundle/manifest.json")
                .status,
            0
        );

        let installed_manifest_path = root.join(".rune/packages/local-update/0.1.0/manifest.json");
        let tampered_manifest = manifest.replace("local-update", "other-package");
        std::fs::write(installed_manifest_path, tampered_manifest).expect("manifest modified");
        let next_manifest = manifest.replace("0.1.0", "0.2.0");
        std::fs::write(&manifest_path, next_manifest).expect("update manifest written");

        let rejected = session.execute_line("pkg update bundle/manifest.json");
        assert_eq!(rejected.status, 1);
        assert!(rejected.stderr.contains("installed manifest name mismatch"));
        assert!(root
            .join(".rune/packages/local-update/0.1.0/manifest.json")
            .exists());
        assert!(!root.join(".rune/packages/local-update/0.2.0").exists());

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_a_verified_rune_script_from_a_local_package() {
        let root = test_root();
        let package_root = root.join("script-bundle/bin");
        std::fs::create_dir_all(&package_root).expect("package directories created");
        let script = b"cat -\necho \"$0|$1|$2|$#\"\n";
        let digest = rune_package::sha256_hex(script);
        std::fs::write(package_root.join("hello.rune"), script).expect("script written");
        let manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-script",
                "version": "0.1.0",
                "description": "A local Rune script package",
                "files": [{{"path": "bin/hello.rune", "sha256": "{digest}"}}],
                "commands": [{{"name": "local-script", "entry": "bin/hello.rune"}}]
            }}"#
        );
        std::fs::write(root.join("script-bundle/manifest.json"), manifest)
            .expect("manifest written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let installed = session.execute_line("pkg install script-bundle/manifest.json");
        assert_eq!(installed.status, 0);
        let output = session.execute_line("local-script one two");
        assert_eq!(output.status, 0, "{output:?}");
        assert_eq!(output.stdout, "local-script|one|two|2\n");
        let piped = session.execute_line("echo input | local-script one two");
        assert_eq!(piped.status, 0, "{piped:?}");
        assert_eq!(piped.stdout, "input\nlocal-script|one|two|2\n");

        std::fs::write(
            root.join(".rune/packages/local-script/0.1.0/bin/hello.rune"),
            b"echo tampered\n",
        )
        .expect("installed script modified");
        let tampered = session.execute_line("local-script");
        assert_eq!(tampered.status, 126);
        assert!(tampered.stderr.contains("integrity failure"));

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_a_verified_lua_script_from_a_local_package() {
        let root = test_root();
        let package_root = root.join("lua-bundle/bin");
        std::fs::create_dir_all(&package_root).expect("package directories created");
        let script = br"print(arg[1]); print(rune.stdin)";
        let digest = rune_package::sha256_hex(script);
        std::fs::write(package_root.join("hello.lua"), script).expect("Lua script written");
        let manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-lua",
                "version": "0.1.0",
                "description": "A local Lua package",
                "files": [{{"path": "bin/hello.lua", "sha256": "{digest}"}}],
                "commands": [{{"name": "local-lua", "entry": "bin/hello.lua"}}]
            }}"#
        );
        std::fs::write(root.join("lua-bundle/manifest.json"), manifest).expect("manifest written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let installed = session.execute_line("pkg install lua-bundle/manifest.json");
        assert_eq!(installed.status, 0, "{installed:?}");
        let output = session.execute_line("printf input | local-lua first");
        assert_eq!(output.status, 0, "{output:?}");
        assert_eq!(output.stdout, "first\ninput\n");

        std::fs::write(
            root.join(".rune/packages/local-lua/0.1.0/bin/hello.lua"),
            b"print('tampered')",
        )
        .expect("installed Lua script modified");
        let tampered = session.execute_line("local-lua");
        assert_eq!(tampered.status, 126);
        assert!(tampered.stderr.contains("integrity failure"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_a_verified_javascript_script_from_a_local_package() {
        let root = test_root();
        let package_root = root.join("javascript-bundle/bin");
        std::fs::create_dir_all(&package_root).expect("package directories created");
        let script = br"print(process.argv[1]); console.log(rune.stdin)";
        let digest = rune_package::sha256_hex(script);
        std::fs::write(package_root.join("hello.js"), script).expect("JavaScript script written");
        let manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-javascript",
                "version": "0.1.0",
                "description": "A local JavaScript package",
                "files": [{{"path": "bin/hello.js", "sha256": "{digest}"}}],
                "commands": [{{"name": "local-javascript", "entry": "bin/hello.js"}}]
            }}"#
        );
        std::fs::write(root.join("javascript-bundle/manifest.json"), manifest)
            .expect("manifest written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let installed = session.execute_line("pkg install javascript-bundle/manifest.json");
        assert_eq!(installed.status, 0, "{installed:?}");
        let output = session.execute_line("printf input | local-javascript first");
        assert_eq!(output.status, 0, "{output:?}");
        assert_eq!(output.stdout, "first\ninput\n");

        std::fs::write(
            root.join(".rune/packages/local-javascript/0.1.0/bin/hello.js"),
            b"print('tampered')",
        )
        .expect("installed JavaScript script modified");
        let tampered = session.execute_line("local-javascript");
        assert_eq!(tampered.status, 126);
        assert!(tampered.stderr.contains("integrity failure"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_a_verified_python_script_from_a_local_package() {
        let root = test_root();
        let package_root = root.join("python-bundle/bin");
        std::fs::create_dir_all(&package_root).expect("package directories created");
        let script = br"print(sys.argv[1]); print(rune.stdin); rune.stderr('warning')";
        let digest = rune_package::sha256_hex(script);
        std::fs::write(package_root.join("hello.py"), script).expect("Python script written");
        let manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-python",
                "version": "0.1.0",
                "description": "A local Python package",
                "files": [{{"path": "bin/hello.py", "sha256": "{digest}"}}],
                "commands": [{{"name": "local-python", "entry": "bin/hello.py"}}]
            }}"#
        );
        std::fs::write(root.join("python-bundle/manifest.json"), manifest)
            .expect("manifest written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let installed = session.execute_line("pkg install python-bundle/manifest.json");
        assert_eq!(installed.status, 0, "{installed:?}");
        let output = session.execute_line("printf input | local-python first");
        assert_eq!(output.status, 0, "{output:?}");
        assert_eq!(output.stdout, "first\ninput\n");
        assert_eq!(output.stderr, "warning");

        std::fs::write(
            root.join(".rune/packages/local-python/0.1.0/bin/hello.py"),
            b"print('tampered')",
        )
        .expect("installed Python script modified");
        let tampered = session.execute_line("local-python");
        assert_eq!(tampered.status, 126);
        assert!(tampered.stderr.contains("integrity failure"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn grants_installed_wasm_filesystem_only_with_manifest_permission() {
        let root = test_root();
        let package_root = root.join("bundle/bin");
        std::fs::create_dir_all(&package_root).expect("package directories created");
        let wasm = package_wasm_probe();
        let digest = rune_package::sha256_hex(&wasm);
        std::fs::write(package_root.join("hello.wasm"), &wasm).expect("module written");
        let manifest = format!(
            r#"{{
                "schema_version": 1,
                "name": "local-wasm",
                "version": "0.1.0",
                "description": "A local WASM package",
                "permissions": {{"filesystem": true}},
                "files": [{{"path": "bin/hello.wasm", "sha256": "{digest}"}}],
                "commands": [{{"name": "local-hello", "entry": "bin/hello.wasm"}}]
            }}"#
        );
        std::fs::write(root.join("bundle/manifest.json"), &manifest).expect("manifest written");
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        assert_eq!(
            session
                .execute_line("pkg install bundle/manifest.json")
                .status,
            0
        );
        let output = session.execute_line("local-hello");
        assert_eq!(output.status, 9, "{output:?}");
        assert!(session
            .execute_line("pkg info local-wasm")
            .stdout
            .contains("permissions: filesystem"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn manages_environment_and_status_builtins() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session.execute_line("export GREETING='hello world'").status,
            0
        );
        assert_eq!(
            session.execute_line("echo \"$GREETING\"").stdout,
            "hello world\n"
        );
        assert_eq!(
            session.execute_line("printenv GREETING").stdout,
            "hello world\n"
        );
        assert_eq!(session.execute_line("setenv NUMBER 42").status, 0);
        assert_eq!(session.execute_line("echo $NUMBER").stdout, "42\n");
        assert_eq!(
            session.execute_line("PREFIX=run echo \"$PREFIX\"").stdout,
            "run\n"
        );
        assert_eq!(
            session.environment().get("PREFIX").map(String::as_str),
            Some("run")
        );
        assert_eq!(session.execute_line("FIRST=one SECOND=$FIRST").status, 0);
        assert_eq!(
            session.environment().get("SECOND").map(String::as_str),
            Some("one")
        );
        assert_eq!(session.execute_line("unset GREETING").status, 0);
        assert_eq!(session.execute_line("printenv GREETING").status, 1);
        assert_eq!(
            session.execute_line("true && echo success").stdout,
            "success\n"
        );
        assert_eq!(
            session.execute_line("false || echo recovered").stdout,
            "recovered\n"
        );
        assert!(session
            .execute_line("true || echo skipped")
            .stdout
            .is_empty());
        assert_eq!(session.execute_line("false && echo skipped").status, 1);
        assert_eq!(session.execute_line("echo $?").stdout, "1\n");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn transfers_text_through_an_explicit_clipboard_provider() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("pbpaste").status, 1);

        let value = Arc::new(Mutex::new(String::new()));
        session.set_clipboard_provider(Box::new(RecordingClipboardProvider {
            value: Arc::clone(&value),
        }));
        assert_eq!(session.execute_line("echo copied | pbcopy").status, 0);
        assert_eq!(value.lock().expect("clipboard lock").as_str(), "copied\n");
        assert_eq!(session.execute_line("pbpaste").stdout, "copied\n");
        assert_eq!(session.execute_line("pbcopy unexpected").status, 2);

        *value.lock().expect("clipboard lock") = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        let oversized = session.execute_line("pbpaste");
        assert_eq!(oversized.status, 1);
        assert!(oversized.stderr.contains("clipboard text is"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn compares_bounded_utf8_files_with_unified_diff_statuses() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session.execute_line("printf 'same\\n' > left.txt").status,
            0
        );
        assert_eq!(
            session.execute_line("printf 'same\\n' > right.txt").status,
            0
        );
        assert_eq!(session.execute_line("diff left.txt right.txt").status, 0);
        assert!(session
            .execute_line("printf 'changed\\n' > right.txt")
            .stdout
            .is_empty());
        let changed = session.execute_line("diff -u left.txt right.txt");
        assert_eq!(changed.status, 1);
        assert!(changed.stdout.contains("--- left.txt\n+++ right.txt\n@@\n"));
        assert!(changed.stdout.contains("-same\n+changed\n"));
        assert_eq!(session.execute_line("diff missing.txt right.txt").status, 1);
        let invalid = session.execute_line("diff --bad left.txt right.txt");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("usage: diff"));
        session
            .write_file("binary", &[0xff])
            .expect("binary fixture written");
        let invalid_text = session.execute_line("diff binary right.txt");
        assert_eq!(invalid_text.status, 2);
        assert!(invalid_text.stderr.contains("not valid UTF-8"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_portable_utility_commands() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("echo -n ready").stdout, "ready");
        assert_eq!(session.execute_line("sleep 0").status, 0);
        let date = session.execute_line("date -u +%Y-%m-%d");
        assert_eq!(date.status, 0);
        assert_eq!(date.stdout.len(), 11);
        assert!(date.stdout.ends_with('\n'));
        assert!(date.stdout.as_bytes()[0..4].iter().all(u8::is_ascii_digit));
        assert_eq!(&date.stdout[4..5], "-");
        assert!(date.stdout.as_bytes()[5..7].iter().all(u8::is_ascii_digit));
        assert_eq!(&date.stdout[7..8], "-");
        assert!(date.stdout.as_bytes()[8..10].iter().all(u8::is_ascii_digit));
        assert_eq!(session.execute_line("printf abc | sum").stdout, "16556 1\n");
        assert_eq!(
            session.execute_line("printf abc | sum -s").stdout,
            "294 1\n"
        );
        assert_eq!(session.execute_line("sum --bad").status, 2);
        assert_eq!(
            session
                .execute_line("printf '1 + 2\n(3 * 4) - 5\n2^8\n' | bc")
                .stdout,
            "3\n7\n256\n"
        );
        let division_by_zero = session.execute_line("printf '1 / 0\n' | bc");
        assert_eq!(division_by_zero.status, 1);
        assert!(division_by_zero.stderr.contains("division by zero"));
        let invalid_character = session.execute_line("printf '1 & 2\n' | bc");
        assert_eq!(invalid_character.status, 1);
        assert!(invalid_character
            .stderr
            .contains("expected a statement separator"));
        assert_eq!(session.execute_line("bc -l").status, 2);
        assert_eq!(session.execute_line("sleep 301").status, 2);
        assert_eq!(session.execute_line("sleep nope").status, 2);
        assert_eq!(
            session
                .execute_line("basename ~/notes/readme.md .md")
                .stdout,
            "readme\n"
        );
        assert_eq!(
            session.execute_line("dirname ~/notes/readme.md").stdout,
            "~/notes\n"
        );
        assert_eq!(session.execute_line("mkdir -p tree/nested").status, 0);
        assert_eq!(
            session
                .execute_line("echo hello > tree/nested/value.txt")
                .status,
            0
        );
        assert_eq!(session.execute_line("du tree").stdout, "6\ttree\n");
        let metadata = session.execute_line("stat tree/nested/value.txt");
        assert_eq!(metadata.status, 0);
        assert!(metadata.stdout.contains("Type: file"));
        assert!(metadata.stdout.contains("Size: 6"));
        assert_eq!(
            session.execute_line("echo first | tee note.txt").stdout,
            "first\n"
        );
        assert_eq!(
            session.execute_line("echo second | tee -a note.txt").stdout,
            "second\n"
        );
        assert_eq!(
            session.execute_line("cat note.txt").stdout,
            "first\nsecond\n"
        );
        assert_eq!(
            session.execute_line("echo abca | tr abc xyz").stdout,
            "xyzx\n"
        );
        assert_eq!(
            session.execute_line("echo banana | tr -d a").stdout,
            "bnn\n"
        );
        assert_eq!(
            session.execute_line("echo alpha | grep alpha -").stdout,
            "alpha\n"
        );
        assert_eq!(session.execute_line("echo alpha | cat -").stdout, "alpha\n");
        assert_eq!(session.execute_line("echo Hi | xxd -p").stdout, "48690a\n");
        assert!(session
            .execute_line("echo Hi | xxd")
            .stdout
            .contains("00000000:"));
        assert_eq!(session.execute_line("mkdir empty").status, 0);
        assert_eq!(session.execute_line("rmdir empty").status, 0);
        assert_eq!(session.execute_line("unlink note.txt").status, 0);
        assert_eq!(session.execute_line("cat note.txt").status, 1);
        assert_eq!(session.execute_line("setenv TEMP value").status, 0);
        assert_eq!(session.execute_line("unsetenv TEMP").status, 0);
        assert_eq!(session.execute_line("printenv TEMP").status, 1);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn identifies_bounded_vfs_file_types_and_stdin() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session
            .write_file("text.txt", b"hello\n")
            .expect("text fixture written");
        session
            .write_file("binary.bin", &[0, 0xff, 1])
            .expect("binary fixture written");
        session
            .write_file("archive.zip", b"PK\x03\x04fixture")
            .expect("archive fixture written");
        session
            .write_file("module.wasm", b"\0asm\x01\0\0\0")
            .expect("WASM fixture written");
        assert_eq!(session.execute_line("mkdir empty-dir").status, 0);
        assert_eq!(session.execute_line("touch empty-file").status, 0);
        assert_eq!(
            session.execute_line("file text.txt binary.bin").stdout,
            "text.txt: ASCII text\nbinary.bin: data\n"
        );
        assert_eq!(
            session
                .execute_line("file -b --mime-type archive.zip")
                .stdout,
            "application/zip\n"
        );
        assert_eq!(
            session
                .execute_line("file -b text.txt empty-dir empty-file module.wasm")
                .stdout,
            "ASCII text\ndirectory\nempty\nWebAssembly binary\n"
        );
        assert_eq!(
            session.execute_line("printf 'stdin\\n' | file -").stdout,
            "-: ASCII text\n"
        );
        assert_eq!(session.execute_line("file missing.txt text.txt").status, 1);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn renders_bounded_tree_output_without_following_symlinks() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("mkdir -p src/nested").status, 0);
        assert_eq!(session.execute_line("touch src/note.txt .hidden").status, 0);
        assert_eq!(session.execute_line("ln -s src/note.txt link").status, 0);
        assert_eq!(session.execute_line("ln -s src linkdir").status, 0);

        let tree = session.execute_line("tree .");
        assert_eq!(tree.status, 0);
        assert_eq!(
            tree.stdout,
            ".\n├── link@\n├── linkdir@\n└── src/\n    ├── nested/\n    └── note.txt\n"
        );
        assert_eq!(
            session.execute_line("tree -a -d -L 2 .").stdout,
            ".\n└── src/\n    └── nested/\n"
        );
        assert_eq!(session.execute_line("tree -L 0 .").status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn creates_bounded_exclusive_temporary_files_and_directories() {
        let container = test_root();
        let home = container.join("Documents");
        let library = container.join("Library");
        let temporary = container.join("tmp");
        let filesystem = SandboxedFileSystem::new_with_layout(&home, &library, &temporary)
            .expect("layout created");
        let mut session = Session::new(filesystem);

        let file = session.execute_line("mktemp");
        assert_eq!(file.status, 0);
        let file_path = file.stdout.trim();
        assert!(file_path.starts_with("~/tmp/rune."));
        assert_eq!(session.execute_line(&format!("stat {file_path}")).status, 0);

        let directory = session.execute_line("mktemp -d -t rune-cache");
        assert_eq!(directory.status, 0);
        let directory_path = directory.stdout.trim();
        let metadata = session.execute_line(&format!("stat {directory_path}"));
        assert_eq!(metadata.status, 0);
        assert!(metadata.stdout.contains("Type: directory"));

        let explicit = session.execute_line("mktemp cache.XXXXXX");
        assert_eq!(explicit.status, 0);
        assert!(explicit.stdout.starts_with("cache."));
        assert_eq!(session.execute_line("mktemp -u cache.XXXXXX").status, 2);
        assert_eq!(session.execute_line("mktemp cache.no").status, 2);
        std::fs::remove_dir_all(container).expect("test container removed");
    }

    #[test]
    fn executes_bounded_checksum_and_base64_utilities() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session.execute_line("printf 123456789 | cksum").stdout,
            "930766865 9\n"
        );
        let invalid_cksum = session.execute_line("cksum one two");
        assert_eq!(invalid_cksum.status, 2);
        assert!(invalid_cksum.stderr.contains("usage: cksum"));
        assert_eq!(
            session.execute_line("echo hello | base64").stdout,
            "aGVsbG8K\n"
        );
        assert_eq!(
            session
                .execute_line("echo aGVsbG8= | base64 --decode")
                .stdout,
            "hello"
        );
        assert_eq!(session.execute_line("echo aA== | base64 -d").stdout, "h");
        let invalid_base64 = session.execute_line("printf '!!!!' | base64 -d");
        assert_eq!(invalid_base64.status, 1);
        assert!(invalid_base64.stderr.contains("invalid input character"));
        let invalid_base64_usage = session.execute_line("base64 --bad");
        assert_eq!(invalid_base64_usage.status, 2);
        session
            .write_file("binary.b64", b"AP8=")
            .expect("base64 fixture written");
        let invalid_text = session.execute_line("base64 -d binary.b64");
        assert_eq!(invalid_text.status, 1);
        assert!(invalid_text.stderr.contains("not valid UTF-8"));
        let oversized = vec![b'a'; 768 * 1024 + 1];
        session
            .write_file("oversized.b64", &oversized)
            .expect("oversized base64 fixture written");
        let oversized_output = session.execute_line("base64 oversized.b64");
        assert_eq!(oversized_output.status, 1);
        assert!(oversized_output.stderr.contains("input exceeds"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_md5_utility_with_standard_vectors() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session.execute_line("printf 123456789 | md5").stdout,
            "25f9e794323b453885f5181f1b624d0b  -\n"
        );
        session
            .write_file("digest-input", b"hello")
            .expect("digest fixture written");
        assert_eq!(
            session.execute_line("md5 digest-input").stdout,
            "5d41402abc4b2a76b9719d911017c592  digest-input\n"
        );
        let invalid = session.execute_line("md5 one two");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("usage: md5"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn evaluates_bounded_expr_arithmetic_comparisons_and_text_operations() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("expr 2 + 3 '*' 4").stdout, "14\n");
        assert_eq!(session.execute_line("expr 2 '>' 1").stdout, "1\n");
        assert_eq!(session.execute_line("expr length hello").stdout, "5\n");
        assert_eq!(session.execute_line("expr index hello e").stdout, "2\n");
        assert_eq!(
            session.execute_line("expr substr hello 2 3").stdout,
            "ell\n"
        );
        let false_result = session.execute_line("expr 1 = 2");
        assert_eq!(false_result.stdout, "0\n");
        assert_eq!(false_result.status, 1);
        let division_by_zero = session.execute_line("expr 1 / 0");
        assert_eq!(division_by_zero.status, 2);
        assert!(division_by_zero.stderr.contains("division by zero"));
        let invalid = session.execute_line("expr 1 +");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("missing operand"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_a_bounded_lua_script_through_the_rust_session() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session
            .write_file(
                "script.lua",
                br#"print(arg[1]); print(rune.stdin); rune.stderr("warning")"#,
            )
            .expect("Lua script written");
        let output = session.execute_line("printf input | lua script.lua first");
        assert_eq!(output.stdout, "first\ninput\n");
        assert_eq!(output.stderr, "warning\n");
        assert_eq!(output.status, 0);

        session
            .write_file("unsafe.lua", b"return io.open('outside', 'w')")
            .expect("unsafe Lua script written");
        let unsafe_output = session.execute_line("lua unsafe.lua");
        assert_eq!(unsafe_output.status, 1);
        assert!(unsafe_output.stderr.contains("lua:"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_a_bounded_python_script_through_the_rust_session() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session
            .write_file(
                "script.py",
                br#"print(sys.argv[1]); print(rune.stdin); rune.stderr("warning")"#,
            )
            .expect("Python script written");
        let output = session.execute_line("printf input | python3 script.py first");
        assert_eq!(output.stdout, "first\ninput\n");
        assert_eq!(output.stderr, "warning");
        assert_eq!(output.status, 0);

        session
            .write_file("unsafe.py", b"open('outside', 'w')")
            .expect("unsafe Python script written");
        let unsafe_output = session.execute_line("python3 unsafe.py");
        assert_eq!(unsafe_output.status, 1);
        assert!(unsafe_output.stderr.contains("PermissionError"));

        let inline = session.execute_line(
            "printf input | python3 -c 'print(sys.argv[1]); print(rune.stdin)' inline-arg",
        );
        assert_eq!(inline.stdout, "inline-arg\ninput\n");
        assert_eq!(inline.status, 0);
        let stdin_script =
            session.execute_line("printf 'print(sys.argv[1])' | python3 - stdin-arg");
        assert_eq!(stdin_script.stdout, "stdin-arg\n");
        assert_eq!(stdin_script.status, 0);
        let missing_code = session.execute_line("python3 -c");
        assert_eq!(missing_code.status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_a_bounded_javascript_script_through_the_rust_session() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        session
            .write_file(
                "script.js",
                br#"print(process.argv[1]); console.log(rune.stdin); console.error("warning")"#,
            )
            .expect("JavaScript script written");
        let output = session.execute_line("printf input | jsc script.js first");
        assert_eq!(output.stdout, "first\ninput\n");
        assert_eq!(output.stderr, "warning\n");
        assert_eq!(output.status, 0);

        session
            .write_file(
                "unsafe.js",
                br#"if (typeof os !== "undefined" || typeof std !== "undefined" || typeof require !== "undefined") { throw new Error("host module exposed"); }"#,
            )
            .expect("safe JavaScript script written");
        let safe_output = session.execute_line("jsc unsafe.js");
        assert_eq!(safe_output.status, 0, "{safe_output:?}");
        assert_eq!(session.execute_line("jsc --in-window unsafe.js").status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn routes_host_session_actions_without_mixing_them_into_command_output() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

        let exit = session.execute_line("exit");
        assert_eq!(exit, CommandOutput::success(""));
        assert_eq!(session.take_action(), Some(SessionAction::Exit));
        assert_eq!(session.take_action(), None);

        let invalid = session.execute_line("exit now");
        assert_eq!(invalid.status, 2);
        assert_eq!(session.take_action(), None);

        let exit_sequence = session.execute_line("exit; echo should-not-run");
        assert_eq!(exit_sequence, CommandOutput::success(""));
        assert_eq!(session.take_action(), Some(SessionAction::Exit));

        let new_window = session.execute_line("new-window; echo should-not-run");
        assert_eq!(new_window, CommandOutput::success(""));
        assert_eq!(session.take_action(), Some(SessionAction::NewWindow));
        assert_eq!(
            session.execute_line("which newWindow").stdout,
            "newWindow: builtin\n"
        );

        let pick_folder = session.execute_line("pickFolder");
        assert_eq!(pick_folder, CommandOutput::success(""));
        assert_eq!(session.take_action(), Some(SessionAction::PickFolder));
        assert_eq!(session.take_action(), None);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn exposes_bounded_session_snapshot_without_environment_values() {
        let root = test_root();
        let mut session = Session::restore_with_id(
            SandboxedFileSystem::new(&root).expect("root created"),
            "panel-1",
        )
        .expect("valid session id");

        assert_eq!(session.execute_line("mkdir work && cd work").status, 0);
        assert_eq!(session.execute_line("bookmark project").status, 0);
        assert_eq!(
            session.execute_line("export PRIVATE=do-not-export").status,
            0
        );
        let snapshot = session.snapshot();

        assert_eq!(snapshot.id, "panel-1");
        assert_eq!(snapshot.working_directory, "~/work");
        assert_eq!(snapshot.history_count, 3);
        assert_eq!(snapshot.bookmark_count, 1);
        assert!(snapshot.environment_count >= 1);
        assert_eq!(snapshot.terminal_columns, 120);
        assert_eq!(snapshot.terminal_rows, 4_096);
        assert_eq!(snapshot.last_status, 0);
        let json = snapshot.to_json();
        assert!(json.contains("\"schema_version\":1"));
        assert!(json.contains("\"id\":\"panel-1\""));
        assert!(json.contains("\"working_directory\":\"~/work\""));
        assert!(!json.contains("do-not-export"));

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn lists_bounded_metadata_with_long_and_human_readable_modes() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("mkdir folder").status, 0);
        assert_eq!(session.execute_line("echo hi > note.txt").status, 0);
        assert_eq!(session.execute_line("touch .hidden").status, 0);
        assert_eq!(session.execute_line("ln -s note.txt link").status, 0);

        let long = session.execute_line("ls -l");
        assert_eq!(long.status, 0);
        assert!(long
            .stdout
            .lines()
            .any(|line| line.starts_with("d ") && line.ends_with(" folder/")));
        assert!(long.stdout.contains("-        3 note.txt\n"));
        assert!(long.stdout.contains("l        8 link@\n"));
        assert!(!long.stdout.contains(".hidden"));

        let human = session.execute_line("ls -lh");
        assert_eq!(human.status, 0);
        assert!(human.stdout.contains("3B note.txt\n"));
        assert!(human.stdout.contains("8B link@\n"));

        let all = session.execute_line("ls -A -- .");
        assert_eq!(all.status, 0);
        assert!(all.stdout.contains(".hidden\n"));
        assert_eq!(session.execute_line("ls -- folder").status, 0);
        assert_eq!(session.execute_line("ls -z").status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn reports_portable_identity_and_registered_command_discovery() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session.execute_line("help").stdout,
            session.execute_line("help -l").stdout
        );
        assert_eq!(session.execute_line("uname").stdout, "Rune\n");
        assert_eq!(session.execute_line("uname -sn").stdout, "Rune rune\n");
        assert_eq!(session.execute_line("whoami").stdout, "rune\n");
        assert_eq!(session.execute_line("alias ll=ls").status, 0);
        let discovered = session.execute_line("which ll echo missing");
        assert_eq!(discovered.status, 1);
        assert_eq!(discovered.stdout, "alias ll='ls'\necho: builtin\n");
        assert_eq!(discovered.stderr, "which: missing: not found\n");
        let typed = session.execute_line("type ll echo missing");
        assert_eq!(typed.status, 1);
        assert_eq!(
            typed.stdout,
            "ll is an alias for ls\necho is a Rune builtin\n"
        );
        assert_eq!(typed.stderr, "type: missing: not found\n");
        let compact = session.execute_line("command -v ll echo missing");
        assert_eq!(compact.status, 1);
        assert_eq!(compact.stdout, "alias ll='ls'\necho\n");
        assert_eq!(compact.stderr, "command: missing: not found\n");
        let verbose = session.execute_line("command -V ll echo");
        assert_eq!(verbose.status, 0);
        assert_eq!(
            verbose.stdout,
            "ll is an alias for ls\necho is a Rune builtin\n"
        );
        let executed = session.execute_line("command echo from-command");
        assert_eq!(executed.status, 0);
        assert_eq!(executed.stdout, "from-command\n");
        assert_eq!(session.execute_line("alias echo=false").status, 0);
        assert_eq!(session.execute_line("echo").status, 1);
        let bypassed = session.execute_line("command echo allowed");
        assert_eq!(bypassed.status, 0);
        assert_eq!(bypassed.stdout, "allowed\n");
        assert_eq!(session.execute_line("command").status, 2);

        let apropos = session.execute_line("apropos archive");
        assert_eq!(apropos.status, 0);
        assert!(apropos
            .stdout
            .contains("ar - create, list, or extract bounded ar archives\n"));
        assert!(apropos
            .stdout
            .contains("tar - create, list, or extract bounded USTAR archives\n"));
        let no_match = session.execute_line("apropos nonexistent-keyword");
        assert_eq!(no_match.status, 1);
        assert_eq!(no_match.stdout, "");
        assert!(no_match
            .stderr
            .contains("apropos: nothing appropriate for nonexistent-keyword"));
        assert_eq!(session.execute_line("apropos").status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn expands_bounded_session_aliases_and_rejects_compound_values() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("alias greet='echo hello'").status, 0);
        assert_eq!(session.execute_line("greet Rune").stdout, "hello Rune\n");
        assert_eq!(
            session.execute_line("alias greet").stdout,
            "alias greet=echo hello\n"
        );
        assert_eq!(session.execute_line("alias nested=greet").status, 0);
        assert_eq!(session.execute_line("nested").stdout, "hello\n");

        assert_eq!(
            session
                .execute_line("alias compound='echo one; echo two'")
                .status,
            0
        );
        let compound = session.execute_line("compound");
        assert_eq!(compound.status, 2);
        assert!(compound.stderr.contains("only one command"));

        assert_eq!(session.execute_line("alias loop=loop").status, 0);
        let recursive = session.execute_line("loop");
        assert_eq!(recursive.status, 2);
        assert!(recursive.stderr.contains("expansion exceeded"));

        assert_eq!(session.execute_line("unalias nested greet loop").status, 0);
        assert_eq!(session.execute_line("unalias -a").status, 0);
        assert!(session.aliases().is_empty());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn loads_bounded_profile_before_restoring_directory_without_history_pollution() {
        let root = test_root();
        std::fs::create_dir_all(&root).expect("root created");
        std::fs::write(
            root.join(".rune_profile"),
            b"# comments are ignored\nexport PROFILE_GREETING=from-profile\nalias profile-greeting='echo from-alias'\necho \"$PROFILE_GREETING\"\nmkdir profile-dir\n",
        )
        .expect("profile written");
        let mut session = Session::restore(SandboxedFileSystem::new(&root).expect("root opened"));
        let startup = session.take_startup_output();
        assert_eq!(
            startup.status, 0,
            "startup stdout={:?} stderr={:?}",
            startup.stdout, startup.stderr
        );
        assert_eq!(startup.stdout, "from-profile\n");
        assert_eq!(
            session
                .environment()
                .get("PROFILE_GREETING")
                .map(String::as_str),
            Some("from-profile")
        );
        assert!(!session
            .history()
            .iter()
            .any(|line| line.contains("PROFILE_GREETING")));
        assert_eq!(session.execute_line("ls").stdout, "profile-dir/\n");
        assert_eq!(
            session.execute_line("profile-greeting").stdout,
            "from-alias\n"
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn loads_multiline_profile_script_without_history_pollution() {
        let root = test_root();
        std::fs::create_dir_all(&root).expect("root created");
        std::fs::write(
            root.join(".rune_profile"),
            b"profile_greeting() {\n  echo \"profile:$1\"\n}\nprofile_greeting startup\n",
        )
        .expect("profile written");

        let mut session = Session::restore(SandboxedFileSystem::new(&root).expect("root opened"));
        let startup = session.take_startup_output();
        assert_eq!(
            startup.status, 0,
            "startup stdout={:?} stderr={:?}",
            startup.stdout, startup.stderr
        );
        assert_eq!(startup.stdout, "profile:startup\n");
        assert!(session
            .history()
            .iter()
            .all(|line| !line.contains("profile_greeting")));
        assert_eq!(
            session.execute_line("profile_greeting later").stdout,
            "profile:later\n"
        );

        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn drops_host_actions_from_startup_profiles_before_interactive_use() {
        let root = test_root();
        std::fs::create_dir_all(&root).expect("root created");
        std::fs::write(root.join(".rune_profile"), b"exit\necho should-not-run\n")
            .expect("profile written");

        let mut session = Session::restore(SandboxedFileSystem::new(&root).expect("root opened"));
        let startup = session.take_startup_output();
        assert_eq!(startup.stdout, "");
        assert_eq!(startup.stderr, "");
        assert_eq!(session.take_action(), None);
        assert_eq!(
            session.execute_line("echo interactive").stdout,
            "interactive\n"
        );
        assert_eq!(session.take_action(), None);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn falls_back_to_standard_profile_names_when_rune_profile_is_absent() {
        let root = test_root();
        std::fs::create_dir_all(&root).expect("root created");
        std::fs::write(
            root.join(".profile"),
            b"export STANDARD_PROFILE=loaded\necho standard-profile\n",
        )
        .expect("standard profile written");
        let mut session = Session::restore(SandboxedFileSystem::new(&root).expect("root opened"));
        let startup = session.take_startup_output();
        assert_eq!(startup.status, 0);
        assert_eq!(startup.stdout, "standard-profile\n");
        assert_eq!(
            session
                .environment()
                .get("STANDARD_PROFILE")
                .map(String::as_str),
            Some("loaded")
        );

        std::fs::write(root.join(".rune_profile"), b"echo rune-profile\n")
            .expect("Rune profile written");
        let mut prioritized =
            Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        let prioritized_startup = prioritized.take_startup_output();
        assert_eq!(prioritized_startup.stdout, "rune-profile\n");
        assert!(prioritized.environment().get("STANDARD_PROFILE").is_none());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn composes_text_pipeline_builtins_from_stdin_and_files() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("echo beta > lines.txt").status, 0);
        assert_eq!(session.execute_line("echo alpha >> lines.txt").status, 0);
        assert_eq!(session.execute_line("echo beta >> lines.txt").status, 0);
        assert_eq!(
            session.execute_line("head -n 2 lines.txt").stdout,
            "beta\nalpha\n"
        );
        assert_eq!(
            session.execute_line("tail -1 lines.txt | sort").stdout,
            "beta\n"
        );
        assert_eq!(session.execute_line("echo 10 > numbers.data").status, 0);
        assert_eq!(session.execute_line("echo 2 >> numbers.data").status, 0);
        assert_eq!(session.execute_line("echo 2 >> numbers.data").status, 0);
        assert_eq!(session.execute_line("echo 1 >> numbers.data").status, 0);
        assert_eq!(
            session.execute_line("sort -n numbers.data").stdout,
            "1\n2\n2\n10\n"
        );
        assert_eq!(
            session.execute_line("sort -nru numbers.data").stdout,
            "10\n2\n1\n"
        );
        assert_eq!(
            session.execute_line("grep -i ALPHA lines.txt").stdout,
            "alpha\n"
        );
        assert_eq!(
            session.execute_line("grep -n alpha lines.txt").stdout,
            "2:alpha\n"
        );
        assert_eq!(session.execute_line("grep -c beta lines.txt").stdout, "2\n");
        let invalid_grep = session.execute_line("grep '[' lines.txt");
        assert_eq!(invalid_grep.status, 2);
        assert!(invalid_grep.stderr.contains("invalid regular expression"));
        assert_eq!(
            session.execute_line("sed 's/beta/Rune/g' lines.txt").stdout,
            "Rune\nalpha\nRune\n"
        );
        assert_eq!(
            session
                .execute_line("sed -e 's/beta/Rune/' -e 's/Rune/CORE/' lines.txt")
                .stdout,
            "CORE\nalpha\nCORE\n"
        );
        assert_eq!(
            session
                .execute_line("echo one one | sed -n 's/one/two/gp'")
                .stdout,
            "two two\n"
        );
        assert_eq!(
            session
                .execute_line("printf 'one\\n' | sed -n -e 's/one/two/p' -e 's/two/three/p'")
                .stdout,
            "two\nthree\n"
        );
        assert_eq!(
            session
                .execute_line("printf 'name,age,city\nA,30,Paris\n' | cut -d , -f 1,3")
                .stdout,
            "name,city\nA,Paris\n"
        );
        assert_eq!(
            session.execute_line("printf abcdef | cut -c 2-4").stdout,
            "bcd"
        );
        assert_eq!(
            session
                .execute_line("printf 'header\nvalue,ok\n' | cut -d , -f 1 -s")
                .stdout,
            "value\n"
        );
        let invalid_sed = session.execute_line("sed 's/beta/Rune/z' lines.txt");
        assert_eq!(invalid_sed.status, 2);
        assert!(invalid_sed.stderr.contains("unsupported substitution flag"));
        assert_eq!(session.execute_line("cut -f 0").status, 2);
        assert_eq!(session.execute_line("cut -c 1 -d ,").status, 2);
        assert_eq!(
            session.execute_line("uniq -c lines.txt").stdout,
            "      1 beta\n      1 alpha\n      1 beta\n"
        );
        assert_eq!(session.execute_line("wc -l lines.txt").stdout, "3\n");
        assert_eq!(session.execute_line("echo *.txt").stdout, "lines.txt\n");
        assert_eq!(session.execute_line("echo \"*.txt\"").stdout, "*.txt\n");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn selects_bounded_head_and_tail_line_ranges() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session
                .execute_line("printf 'one\ntwo\nthree\n' > lines.txt")
                .status,
            0
        );
        assert_eq!(
            session.execute_line("head -n +2 lines.txt").stdout,
            "two\nthree\n"
        );
        assert_eq!(
            session.execute_line("head -n -1 lines.txt").stdout,
            "one\ntwo\n"
        );
        assert_eq!(
            session.execute_line("tail --lines=+2 lines.txt").stdout,
            "two\nthree\n"
        );
        assert_eq!(
            session.execute_line("tail -n -2 lines.txt").stdout,
            "two\nthree\n"
        );
        assert_eq!(
            session.execute_line("head -- lines.txt").stdout,
            "one\ntwo\nthree\n"
        );
        assert_eq!(session.execute_line("head -n invalid lines.txt").status, 2);
        assert_eq!(session.execute_line("tail -z lines.txt").status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn executes_bounded_regular_expression_text_modes() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(session.execute_line("echo beta > lines.txt").status, 0);
        assert_eq!(session.execute_line("echo alpha >> lines.txt").status, 0);
        assert_eq!(session.execute_line("echo beta >> lines.txt").status, 0);
        assert_eq!(
            session.execute_line("grep '^a' lines.txt").stdout,
            "alpha\n"
        );
        assert_eq!(
            session.execute_line("egrep 'alpha|beta' lines.txt").stdout,
            "beta\nalpha\nbeta\n"
        );
        assert_eq!(
            session.execute_line("grep -e '^a' lines.txt").stdout,
            "alpha\n"
        );
        assert_eq!(session.execute_line("echo '[a' > literal.data").status, 0);
        assert_eq!(
            session.execute_line("fgrep '[a' literal.data").stdout,
            "[a\n"
        );
        let invalid_grep = session.execute_line("grep '[' lines.txt");
        assert_eq!(invalid_grep.status, 2);
        assert!(invalid_grep.stderr.contains("invalid regular expression"));
        assert_eq!(
            session
                .execute_line("sed 's/([a-z]+)/[$1]/g' lines.txt")
                .stdout,
            "[beta]\n[alpha]\n[beta]\n"
        );
        let invalid_sed_pattern = session.execute_line("sed 's/[//g' lines.txt");
        assert_eq!(invalid_sed_pattern.status, 2);
        assert!(invalid_sed_pattern
            .stderr
            .contains("invalid regular expression"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn counts_bounded_wc_fields_and_multiple_inputs() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        assert_eq!(
            session
                .execute_line("printf 'é one\\nsecond' > unicode.txt")
                .status,
            0
        );
        assert_eq!(
            session.execute_line("printf 'third\\n' > other.txt").status,
            0
        );
        assert_eq!(session.execute_line("wc -l -- unicode.txt").stdout, "1\n");
        assert_eq!(
            session.execute_line("wc --words -- unicode.txt").stdout,
            "3\n"
        );
        assert_eq!(
            session.execute_line("wc --bytes unicode.txt").stdout,
            "13\n"
        );
        assert_eq!(session.execute_line("wc -m unicode.txt").stdout, "12\n");
        assert_eq!(
            session.execute_line("wc -L -m unicode.txt").stdout,
            "12 6\n"
        );
        assert_eq!(
            session.execute_line("wc -l unicode.txt other.txt").stdout,
            "1 unicode.txt\n1 other.txt\n2 total\n"
        );
        assert_eq!(
            session
                .execute_line("printf 'a\\nb\\n' | wc --lines -- -")
                .stdout,
            "2\n"
        );
        assert_eq!(session.execute_line("wc -z unicode.txt").status, 2);
        std::fs::remove_dir_all(root).expect("root removed");
    }

    #[test]
    fn redacts_environment_assignment_values_before_persisting_history() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
        let output = session.execute_line("export API_TOKEN=super-secret-value");
        assert_eq!(output.status, 0);
        assert_eq!(
            session.environment().get("API_TOKEN").map(String::as_str),
            Some("super-secret-value")
        );
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted environment assignment]")
        );
        let nested = session.execute_line("echo $(export NESTED_TOKEN=nested-secret-value)");
        assert_eq!(nested.status, 0);
        assert!(!session.environment().contains_key("NESTED_TOKEN"));
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted environment assignment]")
        );
        session.persist().expect("history persisted");
        let state = std::fs::read_to_string(root.join(".rune/session.state")).expect("state read");
        assert!(!state.contains("super-secret-value"));

        let output = session.execute_line("setenv SECOND_SECRET another-secret-value");
        assert_eq!(output.status, 0);
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted environment assignment]")
        );
        let output = session.execute_line("SECOND_TOKEN=another-secret-value echo ok");
        assert_eq!(output.status, 0);
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted environment assignment]")
        );
        let output = session.execute_line("ONLY_TOKEN=only-secret-value");
        assert_eq!(output.status, 0);
        assert_eq!(
            session.environment().get("ONLY_TOKEN").map(String::as_str),
            Some("only-secret-value")
        );
        assert_eq!(
            session.history().last().map(String::as_str),
            Some("[redacted environment assignment]")
        );
        session.persist().expect("redacted history persisted");
        let state = std::fs::read_to_string(root.join(".rune/session.state")).expect("state read");
        assert!(!state.contains("only-secret-value"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }
}
