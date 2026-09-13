use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

const MAX_CUT_RANGES: usize = 256;
const MAX_CUT_POSITION: usize = 1_000_000;
const MAX_DIFF_INPUT_BYTES: usize = 1024 * 1024;
const MAX_DIFF_LINES: usize = 4_096;
const MAX_DIFF_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, Copy)]
struct CutRange {
    start: usize,
    end: Option<usize>,
}

enum CutMode {
    Fields {
        delimiter: char,
        suppress_without_delimiter: bool,
        ranges: Vec<CutRange>,
    },
    Characters {
        ranges: Vec<CutRange>,
    },
}

#[derive(Clone, Copy)]
enum GrepCase {
    Sensitive,
    Insensitive,
}

#[derive(Clone, Copy)]
enum GrepMatch {
    Contains,
    Excludes,
}

#[derive(Clone, Copy)]
enum GrepOutput {
    Lines,
    NumberedLines,
    Count,
}

struct GrepOptions {
    case: GrepCase,
    matching: GrepMatch,
    output: GrepOutput,
    pattern: String,
    paths: Vec<String>,
}

pub(super) fn cut(context: &mut CommandContext<'_>) -> CommandOutput {
    let (mode, paths) = match parse_cut_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let text = match read_cut_inputs(context, &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let mut stdout = String::new();
    for line in lines_with_endings(&text) {
        let (body, ending) = line.strip_suffix('\n').map_or((line, ""), |line| {
            line.strip_suffix('\r')
                .map_or((line, "\n"), |line| (line, "\r\n"))
        });
        match &mode {
            CutMode::Fields {
                delimiter,
                suppress_without_delimiter,
                ranges,
            } => {
                if !body.contains(*delimiter) {
                    if !suppress_without_delimiter {
                        stdout.push_str(body);
                        stdout.push_str(ending);
                    }
                    continue;
                }
                let fields = body.split(*delimiter).collect::<Vec<_>>();
                let selected = selected_positions(fields.len(), ranges);
                for (index, position) in selected.into_iter().enumerate() {
                    if index > 0 {
                        stdout.push(*delimiter);
                    }
                    stdout.push_str(fields[position]);
                }
                stdout.push_str(ending);
            }
            CutMode::Characters { ranges } => {
                let characters = body.chars().collect::<Vec<_>>();
                for position in selected_positions(characters.len(), ranges) {
                    stdout.push(characters[position]);
                }
                stdout.push_str(ending);
            }
        }
    }
    CommandOutput::success(stdout)
}

fn parse_cut_args(args: &[String]) -> Result<(CutMode, Vec<String>), CommandOutput> {
    let mut delimiter = '\t';
    let mut delimiter_set = false;
    let mut field_spec = None;
    let mut character_spec = None;
    let mut suppress_without_delimiter = false;
    let mut paths = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument == "--" {
            paths.extend(args[index + 1..].iter().cloned());
            break;
        } else if argument == "-s" {
            suppress_without_delimiter = true;
        } else if argument == "-d" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err(usage(
                    "cut",
                    "usage: cut (-f LIST | -c LIST) [-d CHAR] [-s] [file ...]",
                ));
            };
            delimiter = parse_delimiter(value)?;
            delimiter_set = true;
        } else if let Some(value) = argument.strip_prefix("-d") {
            if value.is_empty() {
                return Err(usage(
                    "cut",
                    "usage: cut (-f LIST | -c LIST) [-d CHAR] [-s] [file ...]",
                ));
            }
            delimiter = parse_delimiter(value)?;
            delimiter_set = true;
        } else if argument == "-f" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err(usage(
                    "cut",
                    "usage: cut (-f LIST | -c LIST) [-d CHAR] [-s] [file ...]",
                ));
            };
            field_spec = Some(value.clone());
        } else if let Some(value) = argument.strip_prefix("-f") {
            if value.is_empty() {
                return Err(usage(
                    "cut",
                    "usage: cut (-f LIST | -c LIST) [-d CHAR] [-s] [file ...]",
                ));
            }
            field_spec = Some(value.to_string());
        } else if argument == "-c" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err(usage(
                    "cut",
                    "usage: cut (-f LIST | -c LIST) [-d CHAR] [-s] [file ...]",
                ));
            };
            character_spec = Some(value.clone());
        } else if let Some(value) = argument.strip_prefix("-c") {
            if value.is_empty() {
                return Err(usage(
                    "cut",
                    "usage: cut (-f LIST | -c LIST) [-d CHAR] [-s] [file ...]",
                ));
            }
            character_spec = Some(value.to_string());
        } else if argument.starts_with('-') {
            return Err(usage(
                "cut",
                "usage: cut (-f LIST | -c LIST) [-d CHAR] [-s] [file ...]",
            ));
        } else {
            paths.push(argument.clone());
        }
        index += 1;
    }

    match (field_spec, character_spec) {
        (Some(spec), None) => Ok((
            CutMode::Fields {
                delimiter,
                suppress_without_delimiter,
                ranges: parse_cut_ranges(&spec)?,
            },
            paths,
        )),
        (None, Some(spec)) if !delimiter_set && !suppress_without_delimiter => Ok((
            CutMode::Characters {
                ranges: parse_cut_ranges(&spec)?,
            },
            paths,
        )),
        (None, Some(_)) => Err(usage("cut", "character mode does not accept -d or -s")),
        (Some(_), Some(_)) | (None, None) => Err(usage("cut", "choose exactly one of -f or -c")),
    }
}

fn parse_delimiter(value: &str) -> Result<char, CommandOutput> {
    let mut characters = value.chars();
    let Some(delimiter) = characters.next() else {
        return Err(usage("cut", "-d requires exactly one character"));
    };
    if characters.next().is_some() {
        return Err(usage("cut", "-d requires exactly one character"));
    }
    Ok(delimiter)
}

fn parse_cut_ranges(spec: &str) -> Result<Vec<CutRange>, CommandOutput> {
    if spec.is_empty() {
        return Err(usage("cut", "LIST must contain positions such as 1,3-5"));
    }
    let pieces = spec.split(',').collect::<Vec<_>>();
    if pieces.len() > MAX_CUT_RANGES {
        return Err(usage("cut", "LIST contains too many ranges"));
    }
    let mut ranges = Vec::with_capacity(pieces.len());
    for piece in pieces {
        let range = if let Some((start, end)) = piece.split_once('-') {
            let start = if start.is_empty() {
                1
            } else {
                parse_cut_position(start)?
            };
            let end = if end.is_empty() {
                None
            } else {
                Some(parse_cut_position(end)?)
            };
            if end.is_some_and(|end| end < start) {
                return Err(usage("cut", "range end must not be before its start"));
            }
            CutRange { start, end }
        } else {
            let position = parse_cut_position(piece)?;
            CutRange {
                start: position,
                end: Some(position),
            }
        };
        ranges.push(range);
    }
    Ok(ranges)
}

fn parse_cut_position(value: &str) -> Result<usize, CommandOutput> {
    let Ok(position) = value.parse::<usize>() else {
        return Err(usage("cut", "LIST positions must be positive integers"));
    };
    if position == 0 || position > MAX_CUT_POSITION {
        return Err(usage("cut", "LIST positions must be between 1 and 1000000"));
    }
    Ok(position)
}

fn selected_positions(length: usize, ranges: &[CutRange]) -> Vec<usize> {
    let mut positions = BTreeSet::new();
    for range in ranges {
        let end = range
            .end
            .unwrap_or(length)
            .min(length)
            .min(MAX_CUT_POSITION);
        if range.start <= end {
            positions.extend(range.start..=end);
        }
    }
    positions.into_iter().map(|position| position - 1).collect()
}

fn read_cut_inputs(
    context: &mut CommandContext<'_>,
    paths: &[String],
) -> Result<String, CommandOutput> {
    if paths.is_empty() {
        return Ok(context.stdin.to_string());
    }
    let mut text = String::new();
    for path in paths {
        if path == "-" {
            text.push_str(context.stdin);
            continue;
        }
        let bytes = context
            .fs
            .read(path)
            .map_err(|error| fs_failure("cut", &error))?;
        text.push_str(&String::from_utf8_lossy(&bytes));
    }
    Ok(text)
}

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
    let options = match parse_grep_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let text = match read_inputs(context, "grep", &options.paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let needle = if matches!(options.case, GrepCase::Insensitive) {
        options.pattern.to_lowercase()
    } else {
        options.pattern.clone()
    };
    let mut stdout = String::new();
    let mut matched = false;
    let mut matching_lines = 0;
    for (index, line) in lines_with_endings(&text).into_iter().enumerate() {
        let original = line_content(line);
        let haystack = if matches!(options.case, GrepCase::Insensitive) {
            original.to_lowercase()
        } else {
            original.to_string()
        };
        let contains = haystack.contains(&needle);
        let selected = match options.matching {
            GrepMatch::Contains => contains,
            GrepMatch::Excludes => !contains,
        };
        if selected {
            matched = true;
            matching_lines += 1;
            if !matches!(options.output, GrepOutput::Count) {
                if matches!(options.output, GrepOutput::NumberedLines) {
                    let _ = write!(stdout, "{}:", index + 1);
                }
                stdout.push_str(line);
            }
        }
    }
    if matches!(options.output, GrepOutput::Count) {
        let _ = writeln!(stdout, "{matching_lines}");
    }
    CommandOutput {
        stdout,
        stderr: String::new(),
        status: i32::from(!matched),
    }
}

pub(super) fn diff(context: &mut CommandContext<'_>) -> CommandOutput {
    let paths = match parse_diff_args(context.args) {
        Ok(paths) => paths,
        Err(output) => return output,
    };
    let left = match read_diff_input(context, &paths[0]) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let right = match read_diff_input(context, &paths[1]) {
        Ok(text) => text,
        Err(output) => return output,
    };
    if left == right {
        return CommandOutput::success("");
    }
    let left_lines = lines_with_endings(&left);
    let right_lines = lines_with_endings(&right);
    if left_lines.len() > MAX_DIFF_LINES || right_lines.len() > MAX_DIFF_LINES {
        return CommandOutput::failure(
            2,
            format!("diff: input exceeds the {MAX_DIFF_LINES}-line comparison limit\n"),
        );
    }
    let cells = (left_lines.len() + 1).saturating_mul(right_lines.len() + 1);
    if cells > MAX_DIFF_CELLS {
        return CommandOutput::failure(
            2,
            format!("diff: comparison exceeds the {MAX_DIFF_CELLS}-cell limit\n"),
        );
    }
    let operations = diff_operations(&left_lines, &right_lines);
    let mut stdout = format!("--- {}\n+++ {}\n@@\n", paths[0], paths[1]);
    for operation in operations {
        match operation {
            DiffOperation::Equal(line) => append_diff_line(&mut stdout, ' ', line),
            DiffOperation::Remove(line) => append_diff_line(&mut stdout, '-', line),
            DiffOperation::Add(line) => append_diff_line(&mut stdout, '+', line),
        }
    }
    CommandOutput {
        stdout,
        stderr: String::new(),
        status: 1,
    }
}

enum DiffOperation<'a> {
    Equal(&'a str),
    Remove(&'a str),
    Add(&'a str),
}

fn parse_diff_args(args: &[String]) -> Result<Vec<String>, CommandOutput> {
    let mut paths = Vec::new();
    let mut parse_options = true;
    for argument in args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && matches!(argument.as_str(), "-u" | "--unified") {
            // The bounded renderer is unified by default.
        } else if parse_options && argument.starts_with('-') {
            return Err(usage("diff", "usage: diff [-u|--unified] FILE1 FILE2"));
        } else {
            paths.push(argument.clone());
        }
    }
    if paths.len() != 2 {
        return Err(usage("diff", "usage: diff [-u|--unified] FILE1 FILE2"));
    }
    Ok(paths)
}

fn read_diff_input(context: &mut CommandContext<'_>, path: &str) -> Result<String, CommandOutput> {
    let bytes = if path == "-" {
        context.stdin.as_bytes().to_vec()
    } else {
        context
            .fs
            .read(path)
            .map_err(|error| fs_failure("diff", &error))?
    };
    if bytes.len() > MAX_DIFF_INPUT_BYTES {
        return Err(CommandOutput::failure(
            2,
            format!("diff: {path}: input exceeds {MAX_DIFF_INPUT_BYTES} bytes\n"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| CommandOutput::failure(2, format!("diff: {path}: input is not valid UTF-8\n")))
}

fn diff_operations<'a>(left: &[&'a str], right: &[&'a str]) -> Vec<DiffOperation<'a>> {
    let mut table = vec![vec![0_u16; right.len() + 1]; left.len() + 1];
    for left_index in (0..left.len()).rev() {
        for right_index in (0..right.len()).rev() {
            table[left_index][right_index] = if left[left_index] == right[right_index] {
                table[left_index + 1][right_index + 1].saturating_add(1)
            } else {
                table[left_index + 1][right_index].max(table[left_index][right_index + 1])
            };
        }
    }

    let mut operations = Vec::with_capacity(left.len() + right.len());
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        if left[left_index] == right[right_index] {
            operations.push(DiffOperation::Equal(left[left_index]));
            left_index += 1;
            right_index += 1;
        } else if table[left_index + 1][right_index] >= table[left_index][right_index + 1] {
            operations.push(DiffOperation::Remove(left[left_index]));
            left_index += 1;
        } else {
            operations.push(DiffOperation::Add(right[right_index]));
            right_index += 1;
        }
    }
    while left_index < left.len() {
        operations.push(DiffOperation::Remove(left[left_index]));
        left_index += 1;
    }
    while right_index < right.len() {
        operations.push(DiffOperation::Add(right[right_index]));
        right_index += 1;
    }
    operations
}

fn append_diff_line(output: &mut String, prefix: char, line: &str) {
    output.push(prefix);
    output.push_str(line);
    if !line.ends_with('\n') {
        output.push('\n');
    }
}

pub(super) fn sed(context: &mut CommandContext<'_>) -> CommandOutput {
    let (suppress_default, script, paths) = match parse_sed_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let script = match parse_substitution(&script) {
        Ok(script) => script,
        Err(message) => return usage("sed", &message),
    };
    let text = match read_inputs(context, "sed", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let mut stdout = String::new();
    for line in lines_with_endings(&text) {
        let (body, ending) = line.strip_suffix('\n').map_or((line, ""), |line| {
            line.strip_suffix('\r')
                .map_or((line, "\n"), |line| (line, "\r\n"))
        });
        let (transformed, matched) = apply_substitution(body, &script);
        if !suppress_default {
            stdout.push_str(&transformed);
            stdout.push_str(ending);
        }
        if script.print_on_match && matched {
            stdout.push_str(&transformed);
            stdout.push_str(ending);
        }
    }
    CommandOutput::success(stdout)
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
        if path == "-" {
            text.push_str(context.stdin);
            continue;
        }
        let bytes = context
            .fs
            .read(path)
            .map_err(|error| fs_failure(command, &error))?;
        text.push_str(&String::from_utf8_lossy(&bytes));
    }
    Ok(text)
}

struct Substitution {
    pattern: String,
    replacement: String,
    global: bool,
    print_on_match: bool,
}

fn parse_sed_args(args: &[String]) -> Result<(bool, String, Vec<String>), CommandOutput> {
    let mut suppress_default = false;
    let mut index = 0;
    if args.first().is_some_and(|argument| argument == "-n") {
        suppress_default = true;
        index += 1;
    }
    let Some(script) = args.get(index) else {
        return Err(usage(
            "sed",
            "usage: sed [-n] 's/PATTERN/REPLACEMENT/[gp]' [file ...]",
        ));
    };
    let paths = args[index + 1..].to_vec();
    Ok((suppress_default, script.clone(), paths))
}

fn parse_substitution(script: &str) -> Result<Substitution, String> {
    let mut characters = script.chars();
    if characters.next() != Some('s') {
        return Err("only s/// substitution scripts are supported".to_string());
    }
    let delimiter = characters
        .next()
        .ok_or_else(|| "substitution is missing its delimiter".to_string())?;
    let pattern = read_script_section(&mut characters, delimiter)?;
    if pattern.is_empty() {
        return Err("substitution pattern must not be empty".to_string());
    }
    let replacement = read_script_section(&mut characters, delimiter)?;
    let flags = characters.collect::<String>();
    let mut global = false;
    let mut print_on_match = false;
    for flag in flags.chars() {
        match flag {
            'g' => global = true,
            'p' => print_on_match = true,
            _ => return Err(format!("unsupported substitution flag: {flag}")),
        }
    }
    Ok(Substitution {
        pattern,
        replacement,
        global,
        print_on_match,
    })
}

fn read_script_section(
    characters: &mut impl Iterator<Item = char>,
    delimiter: char,
) -> Result<String, String> {
    let mut section = String::new();
    let mut escaped = false;
    for character in characters {
        if escaped {
            section.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == delimiter {
            return Ok(section);
        } else {
            section.push(character);
        }
    }
    Err("substitution is missing a delimiter".to_string())
}

fn apply_substitution(line: &str, script: &Substitution) -> (String, bool) {
    if script.global {
        let mut output = String::new();
        let mut remaining = line;
        let mut matched = false;
        while let Some(index) = remaining.find(&script.pattern) {
            matched = true;
            output.push_str(&remaining[..index]);
            output.push_str(&replacement_text(
                &script.replacement,
                &remaining[index..index + script.pattern.len()],
            ));
            remaining = &remaining[index + script.pattern.len()..];
        }
        output.push_str(remaining);
        (output, matched)
    } else if let Some(index) = line.find(&script.pattern) {
        let mut output = String::new();
        output.push_str(&line[..index]);
        output.push_str(&replacement_text(
            &script.replacement,
            &line[index..index + script.pattern.len()],
        ));
        output.push_str(&line[index + script.pattern.len()..]);
        (output, true)
    } else {
        (line.to_string(), false)
    }
}

fn replacement_text(replacement: &str, matched: &str) -> String {
    replacement.replace('&', matched)
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

fn parse_grep_args(args: &[String]) -> Result<GrepOptions, CommandOutput> {
    let mut case = GrepCase::Sensitive;
    let mut matching = GrepMatch::Contains;
    let mut output = GrepOutput::Lines;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-i" => case = GrepCase::Insensitive,
            "-v" => matching = GrepMatch::Excludes,
            "-n" => output = GrepOutput::NumberedLines,
            "-c" => output = GrepOutput::Count,
            "--" => {
                index += 1;
                break;
            }
            _ => break,
        }
        index += 1;
    }
    let Some(pattern) = args.get(index) else {
        return Err(usage(
            "grep",
            "usage: grep [-i] [-v] [-n] [-c] pattern [file ...]",
        ));
    };
    let paths = args[index + 1..].to_vec();
    Ok(GrepOptions {
        case,
        matching,
        output,
        pattern: pattern.clone(),
        paths,
    })
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
