use std::fmt::Write as _;

use crate::{usage, CommandContext, CommandOutput};

pub(super) fn echo(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut stdout = context.args.join(" ");
    stdout.push('\n');
    CommandOutput::success(stdout)
}

pub(super) fn printf(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(format) = context.args.first() else {
        return usage("printf", "usage: printf FORMAT [ARG ...]");
    };
    match format_printf(format, &context.args[1..]) {
        Ok(stdout) => CommandOutput::success(stdout),
        Err(error) => CommandOutput::failure(2, format!("printf: {error}\n")),
    }
}

pub(super) fn pwd(context: &mut CommandContext<'_>) -> CommandOutput {
    if !context.args.is_empty() {
        return usage("pwd", "usage: pwd");
    }
    CommandOutput::success(format!("{}\n", context.fs.current_dir_display()))
}

pub(super) fn uname(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut selected = Vec::new();
    for argument in context.args {
        let Some(flags) = argument.strip_prefix('-') else {
            return usage("uname", "usage: uname [-asnrom]");
        };
        if flags.is_empty() {
            return usage("uname", "usage: uname [-asnrom]");
        }
        for flag in flags.chars() {
            if flag == 'a' {
                selected = vec!['s', 'n', 'r', 'm', 'o'];
                break;
            }
            if !matches!(flag, 's' | 'n' | 'r' | 'm' | 'o') {
                return usage("uname", "usage: uname [-asnrom]");
            }
            if !selected.contains(&flag) {
                selected.push(flag);
            }
        }
    }
    if selected.is_empty() {
        selected.push('s');
    }
    let values = selected
        .into_iter()
        .map(|flag| match flag {
            's' => "Rune".to_string(),
            'n' => "rune".to_string(),
            'r' => env!("CARGO_PKG_VERSION").to_string(),
            'm' => std::env::consts::ARCH.to_string(),
            'o' => std::env::consts::OS.to_string(),
            _ => unreachable!("uname flags are validated above"),
        })
        .collect::<Vec<_>>();
    CommandOutput::success(format!("{}\n", values.join(" ")))
}

pub(super) fn whoami(context: &mut CommandContext<'_>) -> CommandOutput {
    if !context.args.is_empty() {
        return usage("whoami", "usage: whoami");
    }
    // Rune exposes a stable virtual identity instead of leaking a host user
    // name into the portable session or the iOS sandbox.
    CommandOutput::success("rune\n")
}

pub(super) fn which(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("which", "usage: which COMMAND ...");
    }
    let mut output = CommandOutput::success("");
    for name in context.args {
        if let Some(value) = context.aliases.get(name) {
            let _ = writeln!(output.stdout, "alias {name}='{value}'");
        } else if context
            .command_definitions
            .iter()
            .any(|definition| definition.name == name)
        {
            let _ = writeln!(output.stdout, "{name}: builtin");
        } else {
            match crate::find_installed_command_in_filesystem(context.fs, name) {
                Ok(Some(installed_command)) => {
                    let _ = writeln!(
                        output.stdout,
                        "{name}: package {}",
                        installed_command.package
                    );
                }
                Ok(None) => {
                    output.status = 1;
                    let _ = writeln!(output.stderr, "which: {name}: not found");
                }
                Err(error) => {
                    output.status = 1;
                    let _ = writeln!(output.stderr, "which: {name}: {error}");
                }
            }
        }
    }
    output
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
    environment_output(context)
}

pub(super) fn alias(context: &mut CommandContext<'_>) -> CommandOutput {
    match context.args {
        [] => aliases_output(context),
        [definition] => {
            if let Some((name, value)) = definition.split_once('=') {
                if !is_valid_alias_name(name) || value.is_empty() {
                    return usage("alias", "usage: alias [NAME[=VALUE]]");
                }
                context.aliases.insert(name.to_string(), value.to_string());
                CommandOutput::success("")
            } else {
                context.aliases.get(definition).map_or_else(
                    || CommandOutput::failure(1, format!("alias: {definition}: not found\n")),
                    |value| CommandOutput::success(alias_line(definition, value)),
                )
            }
        }
        _ => usage("alias", "usage: alias [NAME[=VALUE]]"),
    }
}

pub(super) fn unalias(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("unalias", "usage: unalias [-a] NAME ...");
    }
    if context.args == ["-a"] {
        context.aliases.clear();
        return CommandOutput::success("");
    }
    for name in context.args {
        if !is_valid_alias_name(name) {
            return usage(
                "unalias",
                "alias names may contain letters, digits, '_', '-', or '.'",
            );
        }
        if context.aliases.remove(name).is_none() {
            return CommandOutput::failure(1, format!("unalias: {name}: not found\n"));
        }
    }
    CommandOutput::success("")
}

pub(super) fn export(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return environment_output(context);
    }
    for argument in context.args {
        let (name, value) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(name, value)| {
                (name, Some(value))
            });
        if !is_valid_variable_name(name) {
            return usage(
                "export",
                "variable names must start with a letter or underscore",
            );
        }
        if let Some(value) = value {
            context.env.insert(name.to_string(), value.to_string());
        } else {
            context.env.entry(name.to_string()).or_default();
        }
    }
    CommandOutput::success("")
}

pub(super) fn unset(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("unset", "usage: unset NAME ...");
    }
    unset_variables(context, "unset")
}

pub(super) fn unsetenv(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("unsetenv", "usage: unsetenv NAME ...");
    }
    unset_variables(context, "unsetenv")
}

fn unset_variables(context: &mut CommandContext<'_>, command: &str) -> CommandOutput {
    for name in context.args {
        if !is_valid_variable_name(name) {
            return usage(
                command,
                "variable names must start with a letter or underscore",
            );
        }
        context.env.remove(name);
    }
    CommandOutput::success("")
}

pub(super) fn printenv(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return environment_output(context);
    }
    let mut stdout = String::new();
    let mut missing = false;
    for name in context.args {
        if let Some(value) = context.env.get(name) {
            stdout.push_str(value);
            stdout.push('\n');
        } else {
            missing = true;
        }
    }
    let status = i32::from(missing);
    CommandOutput {
        stdout,
        stderr: String::new(),
        status,
    }
}

pub(super) fn setenv(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 2 {
        return usage("setenv", "usage: setenv NAME VALUE");
    }
    let name = &context.args[0];
    if !is_valid_variable_name(name) {
        return usage(
            "setenv",
            "variable names must start with a letter or underscore",
        );
    }
    context.env.insert(name.clone(), context.args[1].clone());
    CommandOutput::success("")
}

pub(super) fn true_command(context: &mut CommandContext<'_>) -> CommandOutput {
    if !context.args.is_empty() {
        return usage("true", "usage: true");
    }
    CommandOutput::success("")
}

pub(super) fn false_command(context: &mut CommandContext<'_>) -> CommandOutput {
    if !context.args.is_empty() {
        return usage("false", "usage: false");
    }
    CommandOutput::failure(1, "")
}

pub(super) fn history(context: &mut CommandContext<'_>) -> CommandOutput {
    match context.args {
        [] => history_output(context.history, 0),
        [flag] if flag == "-c" => {
            context.history.clear();
            CommandOutput::success("")
        }
        [subcommand, query] if subcommand == "search" => history_search(context.history, query),
        [subcommand, query, rest @ ..] if subcommand == "search" => {
            let mut terms = Vec::with_capacity(rest.len() + 1);
            terms.push(query.as_str());
            terms.extend(rest.iter().map(String::as_str));
            history_search(context.history, &terms.join(" "))
        }
        [count] => {
            let Ok(count) = count.parse::<usize>() else {
                return usage("history", "usage: history [-c|COUNT|search QUERY ...]");
            };
            let start = context.history.len().saturating_sub(count);
            history_output(context.history, start)
        }
        _ => usage("history", "usage: history [-c|COUNT|search QUERY ...]"),
    }
}

fn history_output(history: &[String], start: usize) -> CommandOutput {
    let mut stdout = String::new();
    for (index, command) in history.iter().enumerate().skip(start) {
        let _ = writeln!(stdout, "{:>5}  {command}", index + 1);
    }
    CommandOutput::success(stdout)
}

fn history_search(history: &[String], query: &str) -> CommandOutput {
    const MAX_QUERY_CHARS: usize = 256;
    if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
        return CommandOutput::failure(
            2,
            format!("history: search query must contain 1-{MAX_QUERY_CHARS} characters\n"),
        );
    }
    let query = query.to_lowercase();
    let mut stdout = String::new();
    for (index, command) in history.iter().enumerate() {
        if command.to_lowercase().contains(&query) {
            let _ = writeln!(stdout, "{:>5}  {command}", index + 1);
        }
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

fn environment_output(context: &CommandContext<'_>) -> CommandOutput {
    let mut stdout = String::new();
    for (name, value) in context.env.iter() {
        stdout.push_str(name);
        stdout.push('=');
        stdout.push_str(value);
        stdout.push('\n');
    }
    CommandOutput::success(stdout)
}

fn format_printf(format: &str, arguments: &[String]) -> Result<String, String> {
    if format.len() > 64 * 1024 {
        return Err("format is too long".to_string());
    }
    let characters = format.chars().collect::<Vec<_>>();
    let mut output = String::new();
    let mut argument_index = 0;
    let mut index = 0;
    while index < characters.len() {
        match characters[index] {
            '\\' => {
                index += 1;
                let Some(escape) = characters.get(index) else {
                    return Err("trailing escape".to_string());
                };
                output.push(match escape {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '\\' => '\\',
                    _ => *escape,
                });
            }
            '%' => {
                index += 1;
                let Some(specifier) = characters.get(index) else {
                    return Err("trailing format marker".to_string());
                };
                match specifier {
                    '%' => output.push('%'),
                    's' => {
                        output.push_str(arguments.get(argument_index).map_or("", String::as_str));
                    }
                    'c' => {
                        if let Some(value) = arguments.get(argument_index) {
                            output.push(value.chars().next().unwrap_or('\0'));
                        }
                    }
                    'd' | 'i' => {
                        let value = arguments
                            .get(argument_index)
                            .ok_or_else(|| "missing integer argument".to_string())?
                            .parse::<i64>()
                            .map_err(|_| "integer argument is invalid".to_string())?;
                        output.push_str(&value.to_string());
                    }
                    other => return Err(format!("unsupported format %{other}")),
                }
                if *specifier != '%' {
                    argument_index += 1;
                }
            }
            character => output.push(character),
        }
        index += 1;
    }
    Ok(output)
}

fn aliases_output(context: &CommandContext<'_>) -> CommandOutput {
    let mut stdout = String::new();
    for (name, value) in context.aliases.iter() {
        stdout.push_str(&alias_line(name, value));
    }
    CommandOutput::success(stdout)
}

fn alias_line(name: &str, value: &str) -> String {
    format!("alias {name}={value}\n")
}

fn is_valid_alias_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

fn is_valid_variable_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(
        characters.next(),
        Some(character) if character == '_' || character.is_ascii_alphabetic()
    ) && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}
