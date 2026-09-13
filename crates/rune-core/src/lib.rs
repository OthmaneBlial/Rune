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
    pub(crate) history: &'a [String],
    pub(crate) command_definitions: &'a [CommandDefinition],
}

/// One independent terminal session.
pub struct Session {
    filesystem: Box<dyn VirtualFileSystem>,
    environment: BTreeMap<String, String>,
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
        self.execute_plan(&plan)
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
            if index > 0 && plan.connectors[index - 1] == Connector::And && output.status != 0 {
                continue;
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
        let program = expand_word(&command.program, &self.environment, self.last_status);
        let arguments: Vec<String> = command
            .arguments
            .iter()
            .map(|word| expand_word(word, &self.environment, self.last_status))
            .collect();
        let mut stdin = external_stdin.to_string();
        let mut stdout_redirect = None;
        let mut stderr_redirect = None;

        for redirection in &command.redirections {
            let (path, append) = match redirection {
                Redirection::Stdin { path } => {
                    let path = expand_word(path, &self.environment, self.last_status);
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
            let path = expand_word(path, &self.environment, self.last_status);
            match redirection {
                Redirection::Stdout { .. } => stdout_redirect = Some((path, append)),
                Redirection::Stderr { .. } => stderr_redirect = Some((path, append)),
                Redirection::Stdin { .. } => unreachable!("stdin redirection handled above"),
            }
        }

        let Some(handler) = self.registry.find(&program) else {
            return CommandOutput::failure(127, format!("{program}: command not found\n"));
        };
        let mut output = {
            let mut context = CommandContext {
                args: &arguments,
                stdin: &stdin,
                fs: self.filesystem.as_mut(),
                env: &mut self.environment,
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

    fn update_pwd(&mut self) {
        self.environment
            .insert("PWD".to_string(), self.filesystem.current_dir_display());
    }
}

fn expand_word(word: &Word, environment: &BTreeMap<String, String>, last_status: i32) -> String {
    let mut expanded = String::new();
    for part in word.parts() {
        match part {
            WordPart::Literal(value) => expanded.push_str(value),
            WordPart::Variable(name) if name == "?" => expanded.push_str(&last_status.to_string()),
            WordPart::Variable(name) => {
                if let Some(value) = environment.get(name) {
                    expanded.push_str(value);
                }
            }
        }
    }
    expanded
}

fn history_entry(line: &str) -> String {
    let Ok(plan) = parse(line) else {
        return line.to_string();
    };
    let contains_environment_setter = plan.pipelines.iter().any(|pipeline| {
        pipeline.commands.iter().any(|command| {
            matches!(
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

pub(crate) fn usage(command: &str, message: &str) -> CommandOutput {
    CommandOutput::failure(2, format!("{command}: {message}\n"))
}

pub(crate) fn fs_failure(command: &str, error: &FsError) -> CommandOutput {
    CommandOutput::failure(1, format!("{command}: {error}\n"))
}

#[cfg(test)]
mod tests {
    use super::Session;
    use rune_fs::SandboxedFileSystem;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root() -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("rune-core-test-{suffix}"))
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
        assert_eq!(session.execute_line("unset GREETING").status, 0);
        assert_eq!(session.execute_line("printenv GREETING").status, 1);
        assert_eq!(
            session.execute_line("true && echo success").stdout,
            "success\n"
        );
        assert_eq!(session.execute_line("false && echo skipped").status, 1);
        assert_eq!(session.execute_line("echo $?").stdout, "1\n");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn loads_bounded_profile_before_restoring_directory_without_history_pollution() {
        let root = test_root();
        std::fs::create_dir_all(&root).expect("root created");
        std::fs::write(
            root.join(".rune_profile"),
            b"# comments are ignored\nexport PROFILE_GREETING=from-profile\necho \"$PROFILE_GREETING\"\nmkdir profile-dir\n",
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
            session.execute_line("uniq -c lines.txt").stdout,
            "      1 beta\n      1 alpha\n      1 beta\n"
        );
        assert_eq!(session.execute_line("wc -l lines.txt").stdout, "3\n");
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
        std::fs::remove_dir_all(root).expect("test root removed");
    }
}
