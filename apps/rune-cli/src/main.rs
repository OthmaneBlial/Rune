use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use rune_core::Session;
use rune_fs::SandboxedFileSystem;

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
    let mut session = Session::new(filesystem);

    if let Some(command) = command {
        let output = session.execute_line(&command);
        print_output(&output.stdout, &output.stderr);
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
}

fn arguments() -> Result<(PathBuf, Option<String>), String> {
    let mut root = std::env::current_dir()
        .map_err(|error| format!("cannot read current directory: {error}"))?;
    let mut command = None;
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
            "-h" | "--help" => {
                println!("usage: rune [--root PATH] [-c COMMAND]");
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}")),
        }
    }
    Ok((root, command))
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
        let output = session.execute_line(&line);
        print_output(&output.stdout, &output.stderr);
    }
    Ok(())
}

fn print_output(stdout: &str, stderr: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(stdout.as_bytes());
    let _ = out.flush();
    let mut err = io::stderr().lock();
    let _ = err.write_all(stderr.as_bytes());
    let _ = err.flush();
}
