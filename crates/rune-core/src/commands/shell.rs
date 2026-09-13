use std::fmt::Write as _;

use crate::{usage, CommandContext, CommandOutput};

pub(super) fn echo(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut stdout = context.args.join(" ");
    stdout.push('\n');
    CommandOutput::success(stdout)
}

pub(super) fn pwd(context: &mut CommandContext<'_>) -> CommandOutput {
    if !context.args.is_empty() {
        return usage("pwd", "usage: pwd");
    }
    CommandOutput::success(format!("{}\n", context.fs.current_dir_display()))
}

pub(super) fn clear(context: &mut CommandContext<'_>) -> CommandOutput {
    if !context.args.is_empty() {
        return usage("clear", "usage: clear");
    }
    CommandOutput::success("\u{1b}[2J\u{1b}[H")
}

pub(super) fn env(context: &mut CommandContext<'_>) -> CommandOutput {
    if !context.args.is_empty() {
        return usage("env", "usage: env");
    }
    let mut stdout = String::new();
    for (name, value) in context.env.iter() {
        stdout.push_str(name);
        stdout.push('=');
        stdout.push_str(value);
        stdout.push('\n');
    }
    CommandOutput::success(stdout)
}

pub(super) fn history(context: &mut CommandContext<'_>) -> CommandOutput {
    if !context.args.is_empty() {
        return usage("history", "usage: history");
    }
    let mut stdout = String::new();
    for (index, command) in context.history.iter().enumerate() {
        let _ = writeln!(stdout, "{:>5}  {command}", index + 1);
    }
    CommandOutput::success(stdout)
}

pub(super) fn help(context: &mut CommandContext<'_>) -> CommandOutput {
    match context.args {
        [] => {
            let mut stdout = String::from("Rune built-in commands:\n");
            for definition in context.command_definitions {
                let _ = writeln!(stdout, "  {:<8} {}", definition.name, definition.summary);
            }
            CommandOutput::success(stdout)
        }
        [name] => context
            .command_definitions
            .iter()
            .find(|definition| definition.name == name)
            .map_or_else(
                || CommandOutput::failure(1, format!("help: unknown command: {name}\n")),
                |definition| {
                    CommandOutput::success(format!(
                        "{} — {}\n",
                        definition.name, definition.summary
                    ))
                },
            ),
        _ => usage("help", "usage: help [command]"),
    }
}
