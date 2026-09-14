use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use rune_core::{CommandEvent, CommandOutput, EventSink, Session, SessionAction};
use rune_fs::SandboxedFileSystem;

struct CliEventSink {
    emitted_output: bool,
}

impl EventSink for CliEventSink {
    fn emit(&mut self, event: CommandEvent) {
        if let CommandEvent::Output { stdout, stderr } = event {
            self.emitted_output = true;
            print_output(&stdout, &stderr);
        }
    }
}

fn main() {
    let (root, command) = match arguments() {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!("rune: {message}");
            std::process::exit(2);
        }
    };
    let filesystem = match SandboxedFileSystem::new(&root) {
        Ok(filesystem) => filesystem,
        Err(error) => {
            eprintln!("rune: cannot initialize root: {error}");
            std::process::exit(1);
        }
    };
    let mut session = Session::restore(filesystem);
    let startup = session.take_startup_output();
    print_output(&startup.stdout, &startup.stderr);

    if let Some(command) = command {
        let output = execute_and_print(&mut session, &command);
        if let Err(error) = session.persist() {
            eprintln!("rune: could not persist session: {error}");
            if output.status == 0 {
                std::process::exit(1);
            }
        }
        if output.status != 0 {
            std::process::exit(output.status);
        }
        return;
    }

    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    if let Err(error) = repl(&mut session, interactive) {
        eprintln!("rune: {error}");
        std::process::exit(1);
    }
    if let Err(error) = session.persist() {
        eprintln!("rune: could not persist session: {error}");
        std::process::exit(1);
    }
}

fn arguments() -> Result<(PathBuf, Option<String>), String> {
    let mut root = std::env::current_dir()
        .map_err(|error| format!("cannot read current directory: {error}"))?;
    let mut command = None;
    let mut script = None;
    let mut script_args = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--root" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--root requires a path".to_string())?;
                root = PathBuf::from(value);
            }
            "-c" | "--command" => {
                command = Some(
                    args.next()
                        .ok_or_else(|| "-c requires a command".to_string())?,
                );
            }
            "--script" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--script requires a virtual path".to_string())?;
                validate_script_argument(&value, "path")?;
                script = Some(value);
            }
            "--script-arg" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--script-arg requires a value".to_string())?;
                validate_script_argument(&value, "value")?;
                script_args.push(value);
            }
            "-h" | "--help" => {
                println!("usage: rune [--root PATH] [-c COMMAND | --script PATH [--script-arg VALUE ...]]");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("rune-cli {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}")),
        }
    }
    if command.is_some() && (script.is_some() || !script_args.is_empty()) {
        return Err("-c/--command and --script are mutually exclusive".to_string());
    }
    if let Some(path) = script {
        let mut source = format!("source {}", quote_shell_word(&path));
        for argument in script_args {
            source.push(' ');
            source.push_str(&quote_shell_word(&argument));
        }
        command = Some(source);
    } else if !script_args.is_empty() {
        return Err("--script-arg requires --script".to_string());
    }
    Ok((root, command))
}

fn validate_script_argument(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(format!(
            "--script {label} must contain printable characters"
        ));
    }
    Ok(())
}

fn quote_shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn repl(session: &mut Session, interactive: bool) -> io::Result<()> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    loop {
        if interactive {
            print!("rune:{}$ ", session.current_directory());
            io::stdout().flush()?;
        }
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let _ = execute_and_print(session, &line);
        if matches!(session.take_action(), Some(SessionAction::Exit)) {
            break;
        }
    }
    Ok(())
}

fn execute_and_print(session: &mut Session, command: &str) -> CommandOutput {
    let mut sink = CliEventSink {
        emitted_output: false,
    };
    let output = session.execute_line_with_events(command, &mut sink);
    if !sink.emitted_output {
        print_output(&output.stdout, &output.stderr);
    }
    output
}

fn print_output(stdout: &str, stderr: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(stdout.as_bytes());
    let _ = out.flush();
    let mut err = io::stderr().lock();
    let _ = err.write_all(stderr.as_bytes());
    let _ = err.flush();
}

#[cfg(test)]
mod tests {
    use super::{quote_shell_word, validate_script_argument};

    #[test]
    fn quotes_script_values_without_shell_expansion() {
        assert_eq!(quote_shell_word("folder's name"), "'folder'\\''s name'");
        assert_eq!(
            quote_shell_word("$HOME && echo unsafe"),
            "'$HOME && echo unsafe'"
        );
    }

    #[test]
    fn rejects_empty_and_control_script_values() {
        assert!(validate_script_argument("", "value").is_err());
        assert!(validate_script_argument("line\nfeed", "value").is_err());
        assert!(validate_script_argument("safe value", "value").is_ok());
    }
}
