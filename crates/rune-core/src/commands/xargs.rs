use crate::{usage, CommandContext, CommandOutput};

const MAX_XARGS_INPUT_BYTES: usize = 1024 * 1024;
const MAX_XARGS_ITEMS: usize = 10_000;
const MAX_XARGS_ARGUMENTS_PER_INVOCATION: usize = 256;

/// The deliberately small, runtime-owned portion of an `xargs` invocation.
/// The command itself is dispatched by `Session` because each generated
/// command must re-enter the normal Rust parser and execution plan.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct XargsPlan {
    pub(crate) command: String,
    pub(crate) initial_arguments: Vec<String>,
    pub(crate) batches: Vec<Vec<String>>,
}

/// Registry metadata for `help` and `which`. Execution is handled by the
/// session so xargs can invoke any built-in or verified installed command.
pub(super) fn xargs(_context: &mut CommandContext<'_>) -> CommandOutput {
    usage(
        "xargs",
        "usage: xargs [-0] [-r] [-n COUNT] [--] [COMMAND [ARG ...]]",
    )
}

pub(crate) fn parse_plan(args: &[String], stdin: &str) -> Result<XargsPlan, String> {
    if stdin.len() > MAX_XARGS_INPUT_BYTES {
        return Err(format!(
            "input exceeds the {MAX_XARGS_INPUT_BYTES}-byte limit"
        ));
    }
    let mut null_delimited = false;
    let mut no_run_if_empty = false;
    let mut max_arguments = MAX_XARGS_ARGUMENTS_PER_INVOCATION;
    let mut command_start = None;
    let mut index = 0;
    let mut parse_options = true;
    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && matches!(argument.as_str(), "-0" | "--null") {
            null_delimited = true;
        } else if parse_options && matches!(argument.as_str(), "-r" | "--no-run-if-empty") {
            no_run_if_empty = true;
        } else if parse_options && argument == "-n" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("-n requires a positive count".to_string());
            };
            max_arguments = parse_max_arguments(value)?;
        } else if parse_options && argument.starts_with("-n") && argument.len() > 2 {
            max_arguments = parse_max_arguments(&argument[2..])?;
        } else if parse_options && argument == "--max-args" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--max-args requires a positive count".to_string());
            };
            max_arguments = parse_max_arguments(value)?;
        } else if parse_options && argument.starts_with("--max-args=") {
            max_arguments = parse_max_arguments(&argument[11..])?;
        } else if parse_options && argument.starts_with('-') {
            return Err(format!("unsupported option: {argument}"));
        } else {
            command_start = Some(index);
            break;
        }
        index += 1;
    }

    let command_arguments = command_start
        .map(|start| args[start..].to_vec())
        .unwrap_or_default();
    let (command, initial_arguments) = if command_arguments.is_empty() {
        ("echo".to_string(), Vec::new())
    } else {
        (
            command_arguments[0].clone(),
            command_arguments[1..].to_vec(),
        )
    };
    if command.is_empty() {
        return Err("command must not be empty".to_string());
    }

    let items = split_items(stdin, null_delimited)?;
    if items.is_empty() && no_run_if_empty {
        return Ok(XargsPlan {
            command,
            initial_arguments,
            batches: Vec::new(),
        });
    }
    let batches = if items.is_empty() {
        vec![Vec::new()]
    } else {
        items
            .chunks(max_arguments)
            .map(<[String]>::to_vec)
            .collect()
    };
    Ok(XargsPlan {
        command,
        initial_arguments,
        batches,
    })
}

fn parse_max_arguments(value: &str) -> Result<usize, String> {
    let count = value
        .parse::<usize>()
        .map_err(|_| "-n/--max-args requires a positive count".to_string())?;
    if count == 0 || count > MAX_XARGS_ARGUMENTS_PER_INVOCATION {
        return Err(format!(
            "argument count must be between 1 and {MAX_XARGS_ARGUMENTS_PER_INVOCATION}"
        ));
    }
    Ok(count)
}

fn split_items(input: &str, null_delimited: bool) -> Result<Vec<String>, String> {
    let items = if null_delimited {
        input
            .split('\0')
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    } else {
        input
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    if items.len() > MAX_XARGS_ITEMS {
        return Err(format!("input contains more than {MAX_XARGS_ITEMS} items"));
    }
    if items.iter().any(|item| item.len() > MAX_XARGS_INPUT_BYTES) {
        return Err(format!(
            "one input item exceeds the {MAX_XARGS_INPUT_BYTES}-byte limit"
        ));
    }
    Ok(items)
}

pub(crate) fn command_line(plan: &XargsPlan, batch: &[String]) -> String {
    let mut words = Vec::with_capacity(2 + plan.initial_arguments.len() + batch.len());
    words.push(quote_word(&plan.command));
    words.extend(
        plan.initial_arguments
            .iter()
            .map(|argument| quote_word(argument)),
    );
    words.extend(batch.iter().map(|argument| quote_word(argument)));
    words.join(" ")
}

fn quote_word(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::{command_line, parse_plan, XargsPlan};

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn batches_whitespace_input_and_preserves_initial_arguments() {
        let args = strings(&["-n", "2", "echo", "prefix"]);
        let plan = parse_plan(&args, "one two\nthree").expect("plan parsed");
        assert_eq!(
            plan,
            XargsPlan {
                command: "echo".to_string(),
                initial_arguments: strings(&["prefix"]),
                batches: vec![strings(&["one", "two"]), strings(&["three"])],
            }
        );
        assert_eq!(
            command_line(&plan, &plan.batches[0]),
            "'echo' 'prefix' 'one' 'two'"
        );
    }

    #[test]
    fn supports_null_delimiters_and_safe_shell_quoting() {
        let args = strings(&["-0", "-r", "echo"]);
        let plan = parse_plan(&args, "a b\0quote'\0").expect("plan parsed");
        assert_eq!(plan.batches, vec![strings(&["a b", "quote'"])]);
        assert_eq!(
            command_line(&plan, &plan.batches[0]),
            "'echo' 'a b' 'quote'\\'''"
        );
    }

    #[test]
    fn skips_empty_input_when_requested() {
        let args = strings(&["-r", "echo"]);
        let plan = parse_plan(&args, " \n").expect("plan parsed");
        assert!(plan.batches.is_empty());
    }
}
