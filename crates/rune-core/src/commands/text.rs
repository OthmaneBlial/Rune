use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

pub(super) fn head(context: &mut CommandContext<'_>) -> CommandOutput {
    let (count, paths) = match parse_count("head", context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let text = match read_inputs(context, "head", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let stdout = lines_with_endings(&text)
        .into_iter()
        .take(count)
        .collect::<String>();
    CommandOutput::success(stdout)
}

pub(super) fn tail(context: &mut CommandContext<'_>) -> CommandOutput {
    let (count, paths) = match parse_count("tail", context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let text = match read_inputs(context, "tail", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let lines = lines_with_endings(&text);
    let start = lines.len().saturating_sub(count);
    CommandOutput::success(lines[start..].concat())
}

pub(super) fn grep(context: &mut CommandContext<'_>) -> CommandOutput {
    let (ignore_case, invert, pattern, paths) = match parse_grep_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let text = match read_inputs(context, "grep", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let needle = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.clone()
    };
    let mut stdout = String::new();
    let mut matched = false;
    for line in lines_with_endings(&text) {
        let original = line_content(line);
        let haystack = if ignore_case {
            original.to_lowercase()
        } else {
            original.to_string()
        };
        let contains = haystack.contains(&needle);
        if contains != invert {
            matched = true;
            stdout.push_str(line);
        }
    }
    CommandOutput {
        stdout,
        stderr: String::new(),
        status: i32::from(!matched),
    }
}

pub(super) fn sort(context: &mut CommandContext<'_>) -> CommandOutput {
    let reverse = match parse_flag("sort", context.args, "r") {
        Ok(reverse) => reverse,
        Err(output) => return output,
    };
    let flag = "-r";
    let paths = context
        .args
        .iter()
        .filter(|argument| argument.as_str() != flag)
        .cloned()
        .collect::<Vec<_>>();
    let text = match read_inputs(context, "sort", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let mut lines = lines_with_endings(&text);
    lines.sort_unstable_by(|left, right| {
        let ordering = line_content(left).cmp(line_content(right));
        if reverse {
            ordering.reverse()
        } else {
            ordering
        }
    });
    CommandOutput::success(lines.concat())
}

pub(super) fn uniq(context: &mut CommandContext<'_>) -> CommandOutput {
    let counted = match parse_flag("uniq", context.args, "c") {
        Ok(counted) => counted,
        Err(output) => return output,
    };
    let flag = "-c";
    let paths = context
        .args
        .iter()
        .filter(|argument| argument.as_str() != flag)
        .cloned()
        .collect::<Vec<_>>();
    let text = match read_inputs(context, "uniq", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let lines = lines_with_endings(&text);
    let mut stdout = String::new();
    let mut start = 0;
    while start < lines.len() {
        let key = line_content(lines[start]);
        let mut end = start + 1;
        while end < lines.len() && line_content(lines[end]) == key {
            end += 1;
        }
        if counted {
            let _ = writeln!(stdout, "{:>7} {key}", end - start);
        } else {
            stdout.push_str(lines[start]);
        }
        start = end;
    }
    CommandOutput::success(stdout)
}

pub(super) fn wc(context: &mut CommandContext<'_>) -> CommandOutput {
    let (lines, words, bytes, paths) = match parse_wc_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let text = match read_inputs(context, "wc", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let mut counts = Vec::new();
    if lines {
        counts.push(lines_with_endings(&text).len().to_string());
    }
    if words {
        counts.push(text.split_whitespace().count().to_string());
    }
    if bytes {
        counts.push(text.len().to_string());
    }
    CommandOutput::success(format!("{}\n", counts.join(" ")))
}

fn read_inputs(
    context: &mut CommandContext<'_>,
    command: &str,
    paths: &[String],
) -> Result<String, CommandOutput> {
    if paths.is_empty() {
        return Ok(context.stdin.to_string());
    }
    let mut text = String::new();
    for path in paths {
        let bytes = context
            .fs
            .read(path)
            .map_err(|error| fs_failure(command, &error))?;
        text.push_str(&String::from_utf8_lossy(&bytes));
    }
    Ok(text)
}

fn lines_with_endings(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split_inclusive('\n').collect()
    }
}

fn line_content(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

fn parse_count(command: &str, args: &[String]) -> Result<(usize, Vec<String>), CommandOutput> {
    let mut count = 10;
    let mut paths = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument == "-n" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err(usage(command, "-n requires a non-negative number"));
            };
            count = parse_nonnegative_count(command, value)?;
        } else if let Some(value) = argument.strip_prefix("-n") {
            count = parse_nonnegative_count(command, value)?;
        } else if argument.len() > 1
            && argument.starts_with('-')
            && argument[1..]
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            count = parse_nonnegative_count(command, &argument[1..])?;
        } else {
            paths.push(argument.clone());
        }
        index += 1;
    }
    Ok((count, paths))
}

fn parse_nonnegative_count(command: &str, value: &str) -> Result<usize, CommandOutput> {
    value
        .parse::<usize>()
        .map_err(|_| usage(command, "-n requires a non-negative number"))
}

fn parse_grep_args(args: &[String]) -> Result<(bool, bool, String, Vec<String>), CommandOutput> {
    let mut ignore_case = false;
    let mut invert = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-i" => ignore_case = true,
            "-v" => invert = true,
            "--" => {
                index += 1;
                break;
            }
            _ => break,
        }
        index += 1;
    }
    let Some(pattern) = args.get(index) else {
        return Err(usage("grep", "usage: grep [-i] [-v] pattern [file ...]"));
    };
    let paths = args[index + 1..].to_vec();
    Ok((ignore_case, invert, pattern.clone(), paths))
}

fn parse_flag(command: &str, args: &[String], flag: &str) -> Result<bool, CommandOutput> {
    let expected = format!("-{flag}");
    for argument in args {
        if argument == &expected {
            continue;
        }
        if argument.starts_with('-') {
            return Err(usage(
                command,
                &format!("usage: {command} [-{flag}] [file ...]"),
            ));
        }
    }
    Ok(args.iter().any(|argument| argument == &expected))
}

fn parse_wc_args(args: &[String]) -> Result<(bool, bool, bool, Vec<String>), CommandOutput> {
    let mut lines = false;
    let mut words = false;
    let mut bytes = false;
    let mut paths = Vec::new();
    for argument in args {
        if let Some(flags) = argument.strip_prefix('-') {
            if flags.is_empty() || !flags.chars().all(|flag| matches!(flag, 'l' | 'w' | 'c')) {
                return Err(usage("wc", "usage: wc [-lwc] [file ...]"));
            }
            lines |= flags.contains('l');
            words |= flags.contains('w');
            bytes |= flags.contains('c');
        } else {
            paths.push(argument.clone());
        }
    }
    if !lines && !words && !bytes {
        lines = true;
        words = true;
        bytes = true;
    }
    Ok((lines, words, bytes, paths))
}
