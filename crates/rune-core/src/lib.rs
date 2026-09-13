//! Portable Rune command engine and session model.
//!
//! The engine is deliberately independent of the native Apple frontend. A
//! future FFI crate can expose its command/event model without moving shell
//! semantics into Swift.

mod commands;
mod persistence;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rune_fs::{FsError, VirtualFileSystem};
use rune_shell::{parse, CommandPlan, Connector, ExecutionPlan, Redirection, Word, WordPart};

const MAX_ALIAS_EXPANSIONS: usize = 32;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const OUTPUT_TRUNCATION_MARKER: &str = "\n[rune: output truncated at 1048576 bytes]\n";

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
    pub(crate) history: &'a [String],
    pub(crate) command_definitions: &'a [CommandDefinition],
}

/// One independent terminal session.
pub struct Session {
    filesystem: Box<dyn VirtualFileSystem>,
    environment: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
    history: Vec<String>,
    history_limit: usize,
    registry: CommandRegistry,
    last_status: i32,
    startup_output: CommandOutput,
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
            history: Vec::new(),
            history_limit: 1_000,
            registry: CommandRegistry::default(),
            last_status: 0,
            startup_output: CommandOutput::success(""),
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
        let state = persistence::load(session.filesystem.as_ref());
        session.load_startup_profile();
        session.history = state.history;
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
        persistence::save(self.filesystem.as_mut(), &directory, &self.history)
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

    /// Executes one parsed command line and returns separate output channels.
    pub fn execute_line(&mut self, input: &str) -> CommandOutput {
        self.execute_line_internal(input, true)
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

        let mut output = if command.program.parts().is_empty() && !command.assignments.is_empty() {
            CommandOutput::success("")
        } else {
            let Some(handler) = self.registry.find(&program) else {
                return CommandOutput::failure(127, format!("{program}: command not found\n"));
            };
            let mut context = CommandContext {
                args: &arguments,
                stdin: &stdin,
                fs: self.filesystem.as_mut(),
                env: &mut self.environment,
                aliases: &mut self.aliases,
                history: &self.history,
                command_definitions: self.registry.definitions(),
            };
            handler(&mut context)
        };
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
        assert_eq!(session.execute_line("pwd").stdout, "~/work\n");
        session.persist().expect("state persisted");
        let restored = Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
        assert_eq!(restored.current_directory(), "~/work");
        assert!(restored.history().contains(&"cat note.txt".to_string()));
        assert_eq!(restored.history().last().map(String::as_str), Some("pwd"));
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
