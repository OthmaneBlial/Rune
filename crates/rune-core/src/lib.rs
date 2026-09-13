//! Portable Rune command engine and session model.
//!
//! The engine is deliberately independent of the native Apple frontend. A
//! FFI consumers can expose its command/event model without moving shell
//! semantics into Swift.

mod clipboard;
mod commands;
mod config;
mod network;
mod persistence;

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

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rune_fs::{FsError, VirtualFileSystem};
use rune_package::PackageManifest;
use rune_runtime::{
    JavaScriptRunner, LuaRunner, PythonRunner, Runtime, RuntimeKind, RuntimeRequest,
};
use rune_shell::{parse, CommandPlan, Connector, ExecutionPlan, Redirection, Word, WordPart};
use rune_wasm::WasmRunner;

const MAX_ALIAS_EXPANSIONS: usize = 32;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_COMPLETION_CANDIDATES: usize = 8;
const MAX_COMPLETION_INPUT_BYTES: usize = 64 * 1024;
const MAX_COMMAND_INPUT_BYTES: usize = 64 * 1024;
const MAX_HISTORY_SEARCH_BYTES: usize = 4 * 1024;
const MAX_INSTALLED_COMMANDS: usize = 4_096;
const MAX_SCRIPT_BYTES: usize = 256 * 1024;
const MAX_SCRIPT_LINES: usize = 1_024;
const MAX_SOURCE_DEPTH: usize = 16;
const MAX_SOURCE_ARGUMENTS: usize = 64;
pub(crate) const CANCELLED_STATUS: i32 = 130;
const MAX_BOOKMARKS: usize = 256;
const MAX_BOOKMARK_NAME_CHARS: usize = 64;
const MAX_BOOKMARK_PATH_BYTES: usize = 64 * 1024;
const MAX_BOOKMARK_BYTES: usize = 256 * 1024;
/// Maximum payload accepted by the explicit native file-transfer boundary.
pub const MAX_FILE_TRANSFER_BYTES: usize = 16 * 1024 * 1024;
const OUTPUT_TRUNCATION_MARKER: &str = "\n[rune: output truncated at 1048576 bytes]\n";
pub(crate) const PACKAGE_INSTALL_ROOT: &str = "~/.rune/packages";

fn supports_path_completion(command: &str) -> bool {
    matches!(
        command,
        "awk"
            | "base64"
            | "bc"
            | "basename"
            | "cat"
            | "cd"
            | "cksum"
            | "cp"
            | "compress"
            | "cut"
            | "curl"
            | "diff"
            | "du"
            | "expr"
            | "find"
            | "grep"
            | "gunzip"
            | "gzip"
            | "head"
            | "jsc"
            | "ls"
            | "ln"
            | "lua"
            | "md5"
            | "mkdir"
            | "mv"
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
            | "touch"
            | "unzip"
            | "uncompress"
            | "unlink"
            | "wasm"
            | "xxd"
            | "zip"
            | "."
    )
}

fn directories_only_for_completion(command: &str) -> bool {
    matches!(command, "cd" | "mkdir" | "rmdir")
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
    /// Output visible after one pipeline has completed. Redirections have
    /// already been applied, so redirected bytes are not emitted here.
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
    pub(crate) config: &'a mut TerminalConfig,
    pub(crate) history: &'a mut Vec<String>,
    pub(crate) command_definitions: &'a [CommandDefinition],
    pub(crate) runtime: &'a dyn Runtime,
    pub(crate) python_runtime: &'a dyn Runtime,
    pub(crate) lua_runtime: &'a dyn Runtime,
    pub(crate) javascript_runtime: &'a dyn Runtime,
    pub(crate) network: &'a dyn NetworkProvider,
    pub(crate) clipboard: &'a dyn ClipboardProvider,
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
    network_provider: Box<dyn NetworkProvider>,
    clipboard_provider: Box<dyn ClipboardProvider>,
    cancellation_requested: Arc<AtomicBool>,
    state_session_id: Option<String>,
    script_parameters: Vec<String>,
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
            network_provider: Box::new(DisabledNetworkProvider),
            clipboard_provider: Box::new(DisabledClipboardProvider),
            cancellation_requested: Arc::new(AtomicBool::new(false)),
            state_session_id: None,
            script_parameters: Vec::new(),
        };
        session.update_pwd();
        session
    }

    /// Restores current directory and command history from the sandbox state.
    ///
    /// Invalid or missing state is ignored and produces a fresh session. The
    /// environment is deliberately never restored from disk.
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
        session.load_startup_profile();
        session.history = state.history;
        session.apply_history_limit();
        session.bookmarks = state.bookmarks;
        if let Some(directory) = state.current_directory {
            let _ = session.filesystem.change_dir(&directory);
        }
        session.update_pwd();
        session.last_status = 0;
        session
    }

    /// Takes output produced while loading `~/.rune_profile` during restore.
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

    /// Returns the bridge handle used to request cancellation safely while
    /// the Rust session is executing on another thread.
    #[must_use]
    pub fn cancellation_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancellation_requested)
    }

    /// Persists only the current virtual directory and command history.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the state directory cannot be
    /// created or the state file cannot be written.
    pub fn persist(&mut self) -> Result<(), FsError> {
        let directory = self.filesystem.current_dir_display();
        persistence::save(
            self.filesystem.as_mut(),
            &directory,
            &self.history,
            &self.bookmarks,
            self.state_session_id.as_deref(),
        )?;
        self.config.save(self.filesystem.as_mut())
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
    /// escaped text, and shell operators are left untouched until the
    /// completion grammar can return a structured replacement range.
    #[must_use]
    pub fn completion_candidates(&self, input: &str) -> Vec<String> {
        if input.len() > MAX_COMPLETION_INPUT_BYTES {
            return Vec::new();
        }
        if input.is_empty()
            || input
                .chars()
                .any(|character| "|;&\\\"'#".contains(character))
        {
            return Vec::new();
        }

        let prefix = input.trim_start();
        if !prefix.chars().any(char::is_whitespace) {
            let mut candidates = self
                .registry
                .definitions()
                .iter()
                .map(|definition| definition.name)
                .filter(|name| *name != prefix && name.starts_with(prefix))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if let Ok(installed_commands) =
                installed_commands_in_filesystem(self.filesystem.as_ref())
            {
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
            return candidates;
        }

        self.path_completion_candidates(input)
    }

    fn path_completion_candidates(&self, input: &str) -> Vec<String> {
        let leading = input.len() - input.trim_start().len();
        let command_end = input[leading..]
            .find(char::is_whitespace)
            .map_or(input.len(), |offset| leading + offset);
        let command = &input[leading..command_end];
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

        let (directory, path_prefix, name_prefix) = match token.rfind('/') {
            Some(slash) => {
                let directory = if slash == 0 { "/" } else { &token[..slash] };
                (directory, &token[..=slash], &token[slash + 1..])
            }
            None => (".", "", token),
        };
        let entries = self.filesystem.list(if path_prefix.is_empty() {
            None
        } else {
            Some(directory)
        });
        let Ok(entries) = entries else {
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

    /// Executes one parsed command line and returns separate output channels.
    pub fn execute_line(&mut self, input: &str) -> CommandOutput {
        let mut sink = NoopEventSink;
        self.execute_line_internal(input, true, 0, "", &mut sink)
    }

    /// Executes one command line and emits bounded output/status events.
    pub fn execute_line_with_events(
        &mut self,
        input: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        self.execute_line_internal(input, true, 0, "", sink)
    }

    /// Executes a bounded newline-delimited automation script.
    ///
    /// Empty lines are ignored. Every non-empty line is sent through the same
    /// parser and command registry as interactive input, and execution
    /// continues after a failed line so automation can observe the complete
    /// output. The returned status is the status of the last executed line.
    pub fn execute_script(&mut self, script: &str) -> CommandOutput {
        let mut sink = NoopEventSink;
        self.execute_script_internal(script, true, 0, "", &mut sink)
    }

    /// Executes a bounded script and emits events for each executed line.
    pub fn execute_script_with_events(
        &mut self,
        script: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
        self.execute_script_internal(script, true, 0, "", sink)
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
        let output = self.execute_script_body(
            script,
            record_history,
            source_depth,
            external_stdin,
            &mut tracking,
        );
        if !tracking.output_emitted && (!output.stdout.is_empty() || !output.stderr.is_empty()) {
            tracking.emit(CommandEvent::Output {
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
            });
        }
        tracking.emit(CommandEvent::Status {
            status: output.status,
            current_directory: self.filesystem.current_dir_display(),
        });
        output
    }

    fn execute_script_body(
        &mut self,
        script: &str,
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        sink: &mut dyn EventSink,
    ) -> CommandOutput {
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
        let mut output = CommandOutput::success("");
        for line in script.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(cancellation) = self.take_cancellation() {
                output.stderr.push_str(&cancellation.stderr);
                output.status = cancellation.status;
                break;
            }
            let line_output = self.execute_line_internal(
                line,
                record_history,
                source_depth,
                external_stdin,
                sink,
            );
            output.stdout.push_str(&line_output.stdout);
            output.stderr.push_str(&line_output.stderr);
            output.status = line_output.status;
            limit_output(&mut output);
        }
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
            tracking.emit(CommandEvent::Output {
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
            });
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
        if record_history {
            let entry = history_entry(line);
            if self.history.last() != Some(&entry) {
                self.history.push(entry);
                persistence::apply_history_limit(&mut self.history, self.history_limit);
            }
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
            config: &mut self.config,
            history: &mut self.history,
            command_definitions: self.registry.definitions(),
            runtime: &self.wasm_runner,
            python_runtime: &self.python_runner,
            lua_runtime: &self.lua_runner,
            javascript_runtime: &self.javascript_runner,
            network: self.network_provider.as_ref(),
            clipboard: self.clipboard_provider.as_ref(),
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
        let mut sink = NoopEventSink;
        for (index, line) in lines.iter().enumerate() {
            let output = self.execute_line_internal(line, false, 0, "", &mut sink);
            self.startup_output.stdout.push_str(&output.stdout);
            self.startup_output.stderr.push_str(&output.stderr);
            if output.status != 0 {
                self.startup_output.status = output.status;
                let _ = writeln!(
                    self.startup_output.stderr,
                    "rune: profile command {} exited with status {}",
                    index + 1,
                    output.status
                );
            }
        }
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
            sink.emit(CommandEvent::Output {
                stdout: event_output.stdout,
                stderr: event_output.stderr,
            });
            output.stdout.push_str(&pipeline_output.stdout);
            output.stderr.push_str(&pipeline_output.stderr);
            output.status = pipeline_output.status;
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

        for assignment in &command.assignments {
            let value = expand_word(
                &assignment.value,
                &self.environment,
                self.last_status,
                &self.script_parameters,
            )
            .value;
            self.environment.insert(assignment.name.clone(), value);
        }
        let (program, arguments) = match self.expand_command_words(command) {
            Ok(expanded) => expanded,
            Err(output) => return output,
        };
        let redirections = match self.apply_redirections(command, &program, external_stdin) {
            Ok(redirections) => redirections,
            Err(output) => return output,
        };

        let source_command = matches!(program.as_str(), "source" | ".");
        let installed_command = if source_command
            || command.program.parts().is_empty()
            || self.registry.find(&program).is_some()
        {
            None
        } else {
            match self.find_installed_command(&program) {
                Ok(command) => command,
                Err(error) => return fs_failure(&program, &error),
            }
        };
        let mut output = if command.program.parts().is_empty() && !command.assignments.is_empty() {
            CommandOutput::success("")
        } else if source_command {
            self.execute_source(
                &program,
                &arguments,
                record_history,
                source_depth,
                &redirections.stdin,
                sink,
            )
        } else if let Some(handler) = self.registry.find(&program) {
            self.execute_builtin(&arguments, &redirections.stdin, handler)
        } else if let Some(installed_command) = installed_command {
            let mut invocation = InstalledInvocation {
                program: &program,
                arguments: &arguments,
                stdin: &redirections.stdin,
                record_history,
                source_depth,
                sink,
                command: &installed_command,
            };
            self.execute_installed(&mut invocation)
        } else {
            CommandOutput::failure(127, format!("{program}: command not found\n"))
        };
        self.apply_history_limit();
        self.update_directory_environment(previous_directory);
        self.apply_output_redirections(&program, &mut output, redirections);
        self.last_status = output.status;
        output
    }

    fn expand_command_words(
        &self,
        command: &CommandPlan,
    ) -> Result<(String, Vec<String>), CommandOutput> {
        let program = expand_word(
            &command.program,
            &self.environment,
            self.last_status,
            &self.script_parameters,
        )
        .value;
        let mut arguments = Vec::new();
        for word in &command.arguments {
            let expanded = expand_word(
                word,
                &self.environment,
                self.last_status,
                &self.script_parameters,
            );
            if expanded.has_wildcard {
                match self.filesystem.glob(&expanded.value) {
                    Ok(matches) => arguments.extend(matches),
                    Err(error) => return Err(fs_failure(&program, &error)),
                }
            } else {
                arguments.push(expanded.value);
            }
        }
        Ok((program, arguments))
    }

    fn execute_source(
        &mut self,
        command: &str,
        arguments: &[String],
        record_history: bool,
        source_depth: usize,
        external_stdin: &str,
        sink: &mut dyn EventSink,
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
        let output = self.execute_script_internal(
            &script,
            record_history,
            source_depth + 1,
            external_stdin,
            sink,
        );
        self.script_parameters = previous_parameters;
        output
    }

    fn apply_redirections(
        &mut self,
        command: &CommandPlan,
        program: &str,
        external_stdin: &str,
    ) -> Result<AppliedRedirections, CommandOutput> {
        let mut stdin = external_stdin.to_string();
        let mut stdout = OutputTarget::Stdout;
        let mut stderr = OutputTarget::Stderr;
        for (descriptor, redirection) in command.redirections.iter().enumerate() {
            match redirection {
                Redirection::Stdin { path } => {
                    let path = expand_word(
                        path,
                        &self.environment,
                        self.last_status,
                        &self.script_parameters,
                    )
                    .value;
                    match self.filesystem.read(&path) {
                        Ok(content) => stdin = String::from_utf8_lossy(&content).into_owned(),
                        Err(error) => return Err(fs_failure(program, &error)),
                    }
                }
                Redirection::Stdout { path, append } => {
                    let path = expand_word(
                        path,
                        &self.environment,
                        self.last_status,
                        &self.script_parameters,
                    )
                    .value;
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
                    let path = expand_word(
                        path,
                        &self.environment,
                        self.last_status,
                        &self.script_parameters,
                    )
                    .value;
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
                    let path = expand_word(
                        path,
                        &self.environment,
                        self.last_status,
                        &self.script_parameters,
                    )
                    .value;
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
        Ok(AppliedRedirections {
            stdin,
            stdout,
            stderr,
        })
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
        }
        self.update_pwd();
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

struct ExpandedWord {
    value: String,
    has_wildcard: bool,
}

fn expand_word(
    word: &Word,
    environment: &BTreeMap<String, String>,
    last_status: i32,
    script_parameters: &[String],
) -> ExpandedWord {
    let mut value = String::new();
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
            WordPart::Wildcard(wildcard) => {
                value.push(*wildcard);
                has_wildcard = true;
            }
        }
    }
    ExpandedWord {
        value,
        has_wildcard,
    }
}

fn history_entry(line: &str) -> String {
    let Ok(plan) = parse(line) else {
        return line.to_string();
    };
    let contains_environment_setter = plan.pipelines.iter().any(|pipeline| {
        pipeline.commands.iter().any(|command| {
            !command.assignments.is_empty()
                || matches!(
                    command.program.literal_value().as_deref(),
                    Some("export" | "setenv")
                )
        })
    });
    let contains_network_request = plan.pipelines.iter().any(|pipeline| {
        pipeline.commands.iter().any(|command| {
            if command.program.literal_value().as_deref() == Some("curl") {
                return true;
            }
            command.program.literal_value().as_deref() == Some("pkg")
                && command.arguments.iter().any(|argument| {
                    matches!(
                        argument.literal_value().as_deref(),
                        Some("--registry" | "--remote")
                    )
                })
        })
    });
    if contains_network_request {
        return "[redacted network command]".to_string();
    }
    if contains_environment_setter {
        "[redacted environment assignment]".to_string()
    } else {
        line.to_string()
    }
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
        persistence::MAX_HISTORY_BYTES, ClipboardError, ClipboardProvider, CommandEvent, EventSink,
        NetworkError, NetworkMethod, NetworkProvider, NetworkRequest, NetworkResponse, Session,
        TerminalConfig, CANCELLED_STATUS, MAX_BOOKMARKS, MAX_BOOKMARK_NAME_CHARS,
        MAX_CLIPBOARD_BYTES, MAX_COMMAND_INPUT_BYTES, MAX_FILE_TRANSFER_BYTES, MAX_OUTPUT_BYTES,
        MAX_SCRIPT_BYTES, MAX_SCRIPT_LINES, MAX_SOURCE_DEPTH, OUTPUT_TRUNCATION_MARKER,
    };
    use rune_fs::SandboxedFileSystem;
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

        let disabled = Session::new(SandboxedFileSystem::new(&root).expect("root reopened"))
            .execute_line("curl https://example.test");
        assert_eq!(disabled.status, 1);
        assert!(disabled.stderr.contains("network provider is unavailable"));
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
    fn creates_and_extracts_a_bounded_stored_zip_archive() {
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
        assert_eq!(
            session.execute_line("unzip bundle.zip ../outside").status,
            1
        );
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

        let ended = session.execute_line("awk 'END { print \"done\" }' rows.csv");
        assert_eq!(ended.status, 0);
        assert_eq!(ended.stdout, "done\n");
        assert_eq!(session.execute_line("awk -F, 'next' rows.csv").status, 2);
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

        assert_eq!(session.execute_line("rm -r source").status, 0);
        let extracted = session.execute_line("tar -xf bundle.tar -C restored");
        assert_eq!(extracted.status, 0);
        assert_eq!(
            session
                .execute_line("cat restored/source/nested/note.txt")
                .stdout,
            "tar-data\n"
        );
        assert_eq!(
            session
                .execute_line("tar -czf compressed.tar source")
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
        assert!(session.completion_candidates("echo ").is_empty());
        assert!(session.completion_candidates("ec | ca").is_empty());
        assert!(session
            .completion_candidates(&"e".repeat(64 * 1024 + 1))
            .is_empty());
        assert_eq!(session.execute_line("mkdir docs").status, 0);
        assert_eq!(session.execute_line("echo notes > docs/notes.md").status, 0);
        assert_eq!(session.completion_candidates("cat do"), vec!["docs/"]);
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
        assert!(restored.history().len() <= 3);
        assert_eq!(restored.execute_line("config reset").status, 0);
        assert_eq!(restored.configuration().history_limit(), 1_000);
        assert_eq!(restored.configuration().scrollback_limit(), 4_096);
        assert!(restored.configuration().toolbar_visible());
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
        let mut config = super::TerminalConfig::default();
        let mut history = Vec::new();
        let registry = super::CommandRegistry::default();
        let runtime = rune_wasm::WasmRunner::default();
        let network = super::DisabledNetworkProvider;
        let clipboard = super::DisabledClipboardProvider;
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
                config: &mut config,
                history: &mut history,
                command_definitions: registry.definitions(),
                runtime: &runtime,
                python_runtime: &runtime,
                lua_runtime: &runtime,
                javascript_runtime: &runtime,
                network: &network,
                clipboard: &clipboard,
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
        let mut config = super::TerminalConfig::default();
        let mut history = Vec::new();
        let registry = super::CommandRegistry::default();
        let runtime = rune_wasm::WasmRunner::default();
        let network = super::DisabledNetworkProvider;
        let clipboard = super::DisabledClipboardProvider;
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
            config: &mut config,
            history: &mut history,
            command_definitions: registry.definitions(),
            runtime: &runtime,
            python_runtime: &runtime,
            lua_runtime: &runtime,
            javascript_runtime: &runtime,
            network: &network,
            clipboard: &clipboard,
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
        let invalid = session.execute_line("printf '%d' nope");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("integer argument is invalid"));
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
        let missing = session.execute_line("find missing");
        assert_eq!(missing.status, 1);
        assert!(missing.stderr.contains("no such file or directory"));
        let invalid = session.execute_line("find project -maxdepth many");
        assert_eq!(invalid.status, 2);
        assert!(invalid.stderr.contains("non-negative number"));
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
        assert_eq!(
            session.execute_line("grep -i ALPHA lines.txt").stdout,
            "alpha\n"
        );
        assert_eq!(
            session.execute_line("grep -n alpha lines.txt").stdout,
            "2:alpha\n"
        );
        assert_eq!(session.execute_line("grep -c beta lines.txt").stdout, "2\n");
        assert_eq!(
            session.execute_line("sed 's/beta/Rune/g' lines.txt").stdout,
            "Rune\nalpha\nRune\n"
        );
        assert_eq!(
            session
                .execute_line("echo one one | sed -n 's/one/two/gp'")
                .stdout,
            "two two\n"
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
