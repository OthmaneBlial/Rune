//! Portable Rune command engine and session model.
//!
//! The engine is deliberately independent of the native Apple frontend. A
//! future FFI crate can expose its command/event model without moving shell
//! semantics into Swift.

mod commands;
mod config;
mod persistence;

pub use config::{TerminalConfig, TerminalTheme};

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rune_fs::{FsError, VirtualFileSystem};
use rune_package::PackageManifest;
use rune_runtime::{Runtime, RuntimeKind, RuntimeRequest};
use rune_shell::{parse, CommandPlan, Connector, ExecutionPlan, Redirection, Word, WordPart};
use rune_wasm::WasmRunner;

const MAX_ALIAS_EXPANSIONS: usize = 32;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_COMPLETION_CANDIDATES: usize = 8;
const MAX_COMPLETION_INPUT_BYTES: usize = 64 * 1024;
const MAX_INSTALLED_COMMANDS: usize = 4_096;
const OUTPUT_TRUNCATION_MARKER: &str = "\n[rune: output truncated at 1048576 bytes]\n";
pub(crate) const PACKAGE_INSTALL_ROOT: &str = "~/.rune/packages";

fn supports_path_completion(command: &str) -> bool {
    matches!(
        command,
        "basename"
            | "cat"
            | "cd"
            | "cp"
            | "du"
            | "find"
            | "grep"
            | "head"
            | "ls"
            | "ln"
            | "mkdir"
            | "mv"
            | "readlink"
            | "rm"
            | "rmdir"
            | "sed"
            | "stat"
            | "tail"
            | "tee"
            | "touch"
            | "unlink"
            | "wasm"
            | "xxd"
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
        };
        session.update_pwd();
        session
    }

    /// Restores current directory and command history from the sandbox state.
    ///
    /// Invalid or missing state is ignored and produces a fresh session. The
    /// environment is deliberately never restored from disk.
    pub fn restore(filesystem: impl VirtualFileSystem + 'static) -> Self {
        let mut session = Self::new(filesystem);
        session.config = TerminalConfig::load(session.filesystem.as_ref());
        session.history_limit = session.config.history_limit();
        let state = persistence::load(session.filesystem.as_ref());
        session.load_startup_profile();
        session.history = state.history;
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

    /// Returns the command history in execution order.
    #[must_use]
    pub fn history(&self) -> &[String] {
        &self.history
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
                .any(|character| "|;&<>\\\"'#".contains(character))
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
        if !supports_path_completion(command) {
            return Vec::new();
        }

        let token_start = input
            .char_indices()
            .rev()
            .find(|(_, character)| character.is_whitespace())
            .map_or(0, |(offset, character)| offset + character.len_utf8());
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
        self.execute_line_internal(input, true)
    }

    /// Executes a bounded newline-delimited automation script.
    ///
    /// Empty lines are ignored. Every non-empty line is sent through the same
    /// parser and command registry as interactive input, and execution
    /// continues after a failed line so automation can observe the complete
    /// output. The returned status is the status of the last executed line.
    pub fn execute_script(&mut self, script: &str) -> CommandOutput {
        let mut output = CommandOutput::success("");
        let mut executed = false;
        for line in script.lines() {
            if line.trim().is_empty() {
                continue;
            }
            executed = true;
            let line_output = self.execute_line(line);
            output.stdout.push_str(&line_output.stdout);
            output.stderr.push_str(&line_output.stderr);
            output.status = line_output.status;
        }
        if executed {
            limit_output(&mut output);
        }
        output
    }

    fn execute_line_internal(&mut self, input: &str, record_history: bool) -> CommandOutput {
        let line = input.trim_matches(['\r', '\n', ' ']);
        if line.is_empty() {
            return CommandOutput::success("");
        }
        if record_history {
            self.history.push(history_entry(line));
            if self.history.len() > self.history_limit {
                let excess = self.history.len() - self.history_limit;
                self.history.drain(0..excess);
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
        let mut output = self.execute_plan(&plan);
        limit_output(&mut output);
        output
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
        for (index, line) in lines.iter().enumerate() {
            let output = self.execute_line_internal(line, false);
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

    fn execute_plan(&mut self, plan: &ExecutionPlan) -> CommandOutput {
        let mut output = CommandOutput::success("");
        for (index, pipeline) in plan.pipelines.iter().enumerate() {
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
            let pipeline_output = self.execute_pipeline(pipeline);
            output.stdout.push_str(&pipeline_output.stdout);
            output.stderr.push_str(&pipeline_output.stderr);
            output.status = pipeline_output.status;
        }
        self.last_status = output.status;
        output
    }

    fn execute_pipeline(&mut self, pipeline: &rune_shell::PipelinePlan) -> CommandOutput {
        let mut stdin = String::new();
        let mut stderr = String::new();
        let mut status = 0;
        for command in &pipeline.commands {
            let result = self.execute_command(command, &stdin);
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

    fn execute_command(&mut self, command: &CommandPlan, external_stdin: &str) -> CommandOutput {
        self.execute_command_with_aliases(command, external_stdin, 0)
    }

    fn execute_command_with_aliases(
        &mut self,
        command: &CommandPlan,
        external_stdin: &str,
        depth: usize,
    ) -> CommandOutput {
        let expanded_command = match self.expand_alias(command, depth) {
            Ok(expanded) => expanded,
            Err(error) => return CommandOutput::failure(2, format!("rune: alias: {error}\n")),
        };
        if let Some(expanded_command) = expanded_command {
            return self.execute_command_with_aliases(&expanded_command, external_stdin, depth + 1);
        }

        for assignment in &command.assignments {
            let value = expand_word(&assignment.value, &self.environment, self.last_status).value;
            self.environment.insert(assignment.name.clone(), value);
        }
        let program = expand_word(&command.program, &self.environment, self.last_status).value;
        let mut arguments = Vec::new();
        for word in &command.arguments {
            let expanded = expand_word(word, &self.environment, self.last_status);
            if expanded.has_wildcard {
                match self.filesystem.glob(&expanded.value) {
                    Ok(matches) => arguments.extend(matches),
                    Err(error) => return fs_failure(&program, &error),
                }
            } else {
                arguments.push(expanded.value);
            }
        }
        let mut stdin = external_stdin.to_string();
        let mut stdout_redirect = None;
        let mut stderr_redirect = None;

        for redirection in &command.redirections {
            let (path, append) = match redirection {
                Redirection::Stdin { path } => {
                    let path = expand_word(path, &self.environment, self.last_status).value;
                    match self.filesystem.read(&path) {
                        Ok(content) => stdin = String::from_utf8_lossy(&content).into_owned(),
                        Err(error) => {
                            return fs_failure(&program, &error);
                        }
                    }
                    continue;
                }
                Redirection::Stdout { path, append } | Redirection::Stderr { path, append } => {
                    (path, *append)
                }
            };
            let path = expand_word(path, &self.environment, self.last_status).value;
            match redirection {
                Redirection::Stdout { .. } => stdout_redirect = Some((path, append)),
                Redirection::Stderr { .. } => stderr_redirect = Some((path, append)),
                Redirection::Stdin { .. } => unreachable!("stdin redirection handled above"),
            }
        }

        let installed_command =
            if command.program.parts().is_empty() || self.registry.find(&program).is_some() {
                None
            } else {
                match self.find_installed_command(&program) {
                    Ok(command) => command,
                    Err(error) => return fs_failure(&program, &error),
                }
            };
        let mut output = if command.program.parts().is_empty() && !command.assignments.is_empty() {
            CommandOutput::success("")
        } else if let Some(handler) = self.registry.find(&program) {
            let mut context = CommandContext {
                args: &arguments,
                stdin: &stdin,
                fs: self.filesystem.as_mut(),
                env: &mut self.environment,
                aliases: &mut self.aliases,
                bookmarks: &mut self.bookmarks,
                config: &mut self.config,
                history: &mut self.history,
                command_definitions: self.registry.definitions(),
                runtime: &self.wasm_runner,
            };
            handler(&mut context)
        } else if let Some(installed_command) = installed_command {
            self.execute_installed_command(&program, &arguments, &stdin, &installed_command)
        } else {
            CommandOutput::failure(127, format!("{program}: command not found\n"))
        };
        self.apply_history_limit();
        self.update_pwd();

        if let Some((path, append)) = stdout_redirect {
            let content = std::mem::take(&mut output.stdout);
            if let Err(error) = self.filesystem.write(&path, content.as_bytes(), append) {
                output.status = 1;
                let _ = writeln!(output.stderr, "{program}: {error}");
            }
        }
        if let Some((path, append)) = stderr_redirect {
            let content = std::mem::take(&mut output.stderr);
            if let Err(error) = self.filesystem.write(&path, content.as_bytes(), append) {
                output.status = 1;
                let _ = writeln!(output.stderr, "{program}: {error}");
            }
        }
        self.last_status = output.status;
        output
    }

    fn apply_history_limit(&mut self) {
        self.history_limit = self.config.history_limit();
        if self.history.len() <= self.history_limit {
            return;
        }
        let excess = self.history.len() - self.history_limit;
        self.history.drain(0..excess);
    }

    fn find_installed_command(&self, name: &str) -> Result<Option<InstalledCommand>, FsError> {
        find_installed_command_in_filesystem(self.filesystem.as_ref(), name)
    }

    fn execute_installed_command(
        &mut self,
        program: &str,
        arguments: &[String],
        stdin: &str,
        installed_command: &InstalledCommand,
    ) -> CommandOutput {
        if !std::path::Path::new(&installed_command.entry)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"))
        {
            return CommandOutput::failure(
                126,
                format!(
                    "{program}: package {} exposes unsupported entry {}; only WASM is available\n",
                    installed_command.package, installed_command.entry
                ),
            );
        }
        let module = match self.filesystem.read(&installed_command.module_path) {
            Ok(module) => module,
            Err(error) => return fs_failure(program, &error),
        };
        let manifest = match self.filesystem.read(&installed_command.manifest_path) {
            Ok(bytes) => match PackageManifest::parse(&bytes) {
                Ok(manifest) => manifest,
                Err(error) => return package_runtime_failure(program, &error),
            },
            Err(error) => return fs_failure(program, &error),
        };
        if let Err(error) = manifest.verify_file(&installed_command.entry, &module) {
            return package_runtime_failure(program, &error);
        }
        let request = RuntimeRequest::new(
            RuntimeKind::Wasm,
            program,
            &module,
            arguments,
            &self.environment,
            stdin,
        );
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
) -> ExpandedWord {
    let mut value = String::new();
    let mut has_wildcard = false;
    for part in word.parts() {
        match part {
            WordPart::Literal(text) => value.push_str(text),
            WordPart::Variable(name) if name == "?" => value.push_str(&last_status.to_string()),
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
    use super::{Session, MAX_OUTPUT_BYTES, OUTPUT_TRUNCATION_MARKER};
    use rune_fs::SandboxedFileSystem;
    use std::sync::atomic::{AtomicU64, Ordering};
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
        assert_eq!(session.execute_line("cat note-link").stdout, "hello\n");
        assert!(session
            .execute_line("ln -s ../missing.txt dangling-link")
            .stderr
            .contains("no such file or directory"));
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
        assert_eq!(session.execute_line("echo one").status, 0);
        assert_eq!(session.execute_line("echo two").status, 0);
        assert!(session.history().len() <= 3);
        assert_eq!(session.execute_line("config set history-limit 0").status, 2);
        assert_eq!(session.configuration().history_limit(), 3);
        assert_eq!(session.execute_line("config set font-size 33").status, 2);
        assert_eq!(session.configuration().font_size(), 20);
        assert_eq!(session.execute_line("config set theme paper").status, 2);
        assert_eq!(session.configuration().theme().as_str(), "ember");
        session.persist().expect("configuration persisted");
        let mut restored =
            Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert_eq!(restored.configuration().history_limit(), 3);
        assert_eq!(restored.configuration().font_size(), 20);
        assert_eq!(restored.configuration().theme().as_str(), "ember");
        assert!(restored.history().len() <= 3);
        assert_eq!(restored.execute_line("config reset").status, 0);
        assert_eq!(restored.configuration().history_limit(), 1_000);
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
        assert_eq!(session.execute_line("pkg search hello").status, 2);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn installs_lists_runs_and_removes_a_verified_wasm_package() {
        let root = test_root();
        let package_root = root.join("bundle/bin");
        std::fs::create_dir_all(&package_root).expect("package directories created");
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "proc_exit"
                    (func $proc_exit (param i32)))
                  (memory (export "memory") 1)
                  (func (export "_start")
                    (i32.const 7)
                    (call $proc_exit)))
            "#,
        )
        .expect("valid package module");
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
        std::fs::write(root.join("bundle/manifest.json"), manifest).expect("manifest written");
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
    fn executes_bounded_portable_utility_commands() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
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
    fn reports_portable_identity_and_registered_command_discovery() {
        let root = test_root();
        let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));
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
            session.execute_line("sed 's/beta/Rune/g' lines.txt").stdout,
            "Rune\nalpha\nRune\n"
        );
        assert_eq!(
            session
                .execute_line("echo one one | sed -n 's/one/two/gp'")
                .stdout,
            "two two\n"
        );
        let invalid_sed = session.execute_line("sed 's/beta/Rune/z' lines.txt");
        assert_eq!(invalid_sed.status, 2);
        assert!(invalid_sed.stderr.contains("unsupported substitution flag"));
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
