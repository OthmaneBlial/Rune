use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt::Write as _;

use regex::{Regex, RegexBuilder};

use crate::{fs_failure, usage, CommandContext, CommandOutput};

const MAX_CUT_RANGES: usize = 256;
const MAX_CUT_POSITION: usize = 1_000_000;
const MAX_DIFF_INPUT_BYTES: usize = 1024 * 1024;
const MAX_DIFF_LINES: usize = 4_096;
const MAX_DIFF_CELLS: usize = 4_000_000;
const MAX_GREP_PATTERN_BYTES: usize = 16 * 1024;

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
    mode: GrepMode,
    pattern: String,
    paths: Vec<String>,
}

#[derive(Clone, Copy)]
enum GrepMode {
    Regex,
    Fixed,
}

enum GrepMatcher {
    Regex(Regex),
    Fixed {
        needle: String,
        case_insensitive: bool,
    },
}

impl GrepMatcher {
    fn is_match(&self, text: &str) -> bool {
        match self {
            Self::Regex(regex) => regex.is_match(text),
            Self::Fixed {
                needle,
                case_insensitive: true,
            } => text.to_lowercase().contains(needle),
            Self::Fixed {
                needle,
                case_insensitive: false,
            } => text.contains(needle),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SortOptions {
    reverse: bool,
    numeric: bool,
    unique: bool,
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
    let (selection, paths) = match parse_count("head", context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let text = match read_inputs(context, "head", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    CommandOutput::success(select_lines(&text, selection))
}

pub(super) fn tail(context: &mut CommandContext<'_>) -> CommandOutput {
    let (selection, paths) = match parse_count("tail", context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let text = match read_inputs(context, "tail", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    CommandOutput::success(select_lines(&text, selection))
}

pub(super) fn grep(context: &mut CommandContext<'_>) -> CommandOutput {
    grep_with_mode(context, GrepMode::Regex, "grep")
}

pub(super) fn egrep(context: &mut CommandContext<'_>) -> CommandOutput {
    grep_with_mode(context, GrepMode::Regex, "egrep")
}

pub(super) fn fgrep(context: &mut CommandContext<'_>) -> CommandOutput {
    grep_with_mode(context, GrepMode::Fixed, "fgrep")
}

fn grep_with_mode(
    context: &mut CommandContext<'_>,
    default_mode: GrepMode,
    command: &str,
) -> CommandOutput {
    let options = match parse_grep_args(context.args, default_mode, command) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let matcher = match compile_grep_matcher(&options) {
        Ok(matcher) => matcher,
        Err(error) => {
            return CommandOutput::failure(2, format!("{command}: {error}\n"));
        }
    };
    let text = match read_inputs(context, command, &options.paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let mut stdout = String::new();
    let mut found_match = false;
    let mut matching_lines = 0;
    for (index, line) in lines_with_endings(&text).into_iter().enumerate() {
        let original = line_content(line);
        let contains = matcher.is_match(original);
        let selected = match options.matching {
            GrepMatch::Contains => contains,
            GrepMatch::Excludes => !contains,
        };
        if selected {
            found_match = true;
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
        status: i32::from(!found_match),
    }
}

fn compile_grep_matcher(options: &GrepOptions) -> Result<GrepMatcher, String> {
    if options.pattern.len() > MAX_GREP_PATTERN_BYTES {
        return Err(format!(
            "pattern exceeds the {MAX_GREP_PATTERN_BYTES}-byte limit"
        ));
    }
    let case_insensitive = matches!(options.case, GrepCase::Insensitive);
    match options.mode {
        GrepMode::Fixed => Ok(GrepMatcher::Fixed {
            needle: if case_insensitive {
                options.pattern.to_lowercase()
            } else {
                options.pattern.clone()
            },
            case_insensitive,
        }),
        GrepMode::Regex => RegexBuilder::new(&options.pattern)
            .case_insensitive(case_insensitive)
            .build()
            .map(GrepMatcher::Regex)
            .map_err(|error| format!("invalid regular expression: {error}")),
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
    let (suppress_default, scripts, paths) = match parse_sed_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let scripts = match scripts
        .iter()
        .map(|script| parse_substitution(script))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(scripts) => scripts,
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
        let mut transformed = body.to_string();
        let mut prints = Vec::new();
        for script in &scripts {
            let (next, matched) = apply_substitution(&transformed, script);
            transformed = next;
            if script.print_on_match && matched {
                prints.push(transformed.clone());
            }
        }
        if !suppress_default {
            stdout.push_str(&transformed);
            stdout.push_str(ending);
        }
        for printed in prints {
            stdout.push_str(&printed);
            stdout.push_str(ending);
        }
    }
    CommandOutput::success(stdout)
}

pub(super) fn sort(context: &mut CommandContext<'_>) -> CommandOutput {
    let (options, paths) = match parse_sort_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let text = match read_inputs(context, "sort", &paths) {
        Ok(text) => text,
        Err(output) => return output,
    };
    let mut lines = lines_with_endings(&text);
    lines.sort_unstable_by(|left, right| {
        let ordering = if options.numeric {
            numeric_line_order(line_content(left), line_content(right))
        } else {
            line_content(left).cmp(line_content(right))
        };
        if options.reverse {
            ordering.reverse()
        } else {
            ordering
        }
    });
    if options.unique {
        lines.dedup_by(|left, right| line_content(left) == line_content(right));
    }
    CommandOutput::success(lines.concat())
}

fn parse_sort_args(args: &[String]) -> Result<(SortOptions, Vec<String>), CommandOutput> {
    let mut options = SortOptions {
        reverse: false,
        numeric: false,
        unique: false,
    };
    let mut paths = Vec::new();
    let mut parse_options = true;
    for argument in args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument.starts_with('-') {
            let flags = argument.strip_prefix('-').unwrap_or_default();
            if flags.is_empty() || !flags.chars().all(|flag| matches!(flag, 'r' | 'n' | 'u')) {
                return Err(usage("sort", "usage: sort [-nru] [--] [file ...]"));
            }
            options.reverse |= flags.contains('r');
            options.numeric |= flags.contains('n');
            options.unique |= flags.contains('u');
        } else {
            paths.push(argument.clone());
        }
    }
    Ok((options, paths))
}

fn numeric_line_order(left: &str, right: &str) -> Ordering {
    match (parse_numeric_prefix(left), parse_numeric_prefix(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.cmp(right),
    }
}

fn parse_numeric_prefix(line: &str) -> Option<i128> {
    let line = line.trim_start();
    let end = line
        .char_indices()
        .find(|(_, character)| {
            !character.is_ascii_digit() && *character != '+' && *character != '-'
        })
        .map_or(line.len(), |(index, _)| index);
    let value = &line[..end];
    if value.is_empty() || matches!(value, "+" | "-") {
        return None;
    }
    value.parse().ok()
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
    let (options, paths) = match parse_wc_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let input_paths = if paths.is_empty() {
        vec![None]
    } else {
        paths.iter().map(Some).collect()
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut total = WcCounts::default();
    let mut failed = false;
    for path in &input_paths {
        let bytes = match path {
            None => Ok(context.stdin.as_bytes().to_vec()),
            Some(path) if *path == "-" => Ok(context.stdin.as_bytes().to_vec()),
            Some(path) => context
                .fs
                .read(path)
                .map_err(|error| (path.as_str(), error)),
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err((path, error)) => {
                failed = true;
                let _ = writeln!(stderr, "wc: {path}: {error}");
                continue;
            }
        };
        let counts = count_wc_bytes(&bytes, options.fields.contains(WcFields::CHARACTERS));
        total.add(counts);
        let label = path.filter(|_| input_paths.len() > 1).map(String::as_str);
        stdout.push_str(&format_wc_counts(counts, options, label));
    }
    if input_paths.len() > 1 {
        stdout.push_str(&format_wc_counts(total, options, Some("total")));
    }
    CommandOutput {
        stdout,
        stderr,
        status: i32::from(failed),
    }
}

#[derive(Clone, Copy)]
struct WcOptions {
    fields: WcFields,
}

#[derive(Clone, Copy, Default)]
struct WcFields(u8);

impl WcFields {
    const LINES: u8 = 1;
    const WORDS: u8 = 2;
    const BYTES: u8 = 4;
    const CHARACTERS: u8 = 8;
    const MAX_LINE_LENGTH: u8 = 16;

    const fn contains(self, field: u8) -> bool {
        self.0 & field != 0
    }

    fn insert(&mut self, field: u8) {
        self.0 |= field;
    }

    fn remove(&mut self, field: u8) {
        self.0 &= !field;
    }
}

#[derive(Clone, Copy, Default)]
struct WcCounts {
    lines: usize,
    words: usize,
    bytes: usize,
    characters: usize,
    max_line_length: usize,
}

impl WcCounts {
    fn add(&mut self, other: Self) {
        self.lines = self.lines.saturating_add(other.lines);
        self.words = self.words.saturating_add(other.words);
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.characters = self.characters.saturating_add(other.characters);
        self.max_line_length = self.max_line_length.max(other.max_line_length);
    }
}

fn count_wc_bytes(bytes: &[u8], count_characters: bool) -> WcCounts {
    let text = String::from_utf8_lossy(bytes);
    let characters = text.chars().count();
    let max_line_length = if count_characters {
        text.lines()
            .map(str::chars)
            .map(Iterator::count)
            .max()
            .unwrap_or(0)
    } else {
        bytes
            .split(|byte| *byte == b'\n')
            .map(<[u8]>::len)
            .max()
            .unwrap_or(0)
    };
    let mut lines = 0;
    for byte in bytes {
        if *byte == b'\n' {
            lines += 1;
        }
    }
    WcCounts {
        lines,
        words: text.split_whitespace().count(),
        bytes: bytes.len(),
        characters,
        max_line_length,
    }
}

fn format_wc_counts(counts: WcCounts, options: WcOptions, label: Option<&str>) -> String {
    let mut fields = Vec::new();
    if options.fields.contains(WcFields::LINES) {
        fields.push(counts.lines.to_string());
    }
    if options.fields.contains(WcFields::WORDS) {
        fields.push(counts.words.to_string());
    }
    if options.fields.contains(WcFields::BYTES) {
        fields.push(counts.bytes.to_string());
    }
    if options.fields.contains(WcFields::CHARACTERS) {
        fields.push(counts.characters.to_string());
    }
    if options.fields.contains(WcFields::MAX_LINE_LENGTH) {
        fields.push(counts.max_line_length.to_string());
    }
    if let Some(label) = label {
        fields.push(label.to_string());
    }
    format!("{}\n", fields.join(" "))
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
    pattern: Regex,
    replacement: String,
    global: bool,
    print_on_match: bool,
}

fn parse_sed_args(args: &[String]) -> Result<(bool, Vec<String>, Vec<String>), CommandOutput> {
    let mut suppress_default = false;
    let mut scripts = Vec::new();
    let mut paths = Vec::new();
    let mut parse_options = true;
    let mut expression_option_seen = false;
    let mut positional_script_seen = false;
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "-n" {
            suppress_default = true;
        } else if parse_options && matches!(argument.as_str(), "-e" | "--expression") {
            index += 1;
            let Some(script) = args.get(index) else {
                return Err(sed_usage());
            };
            scripts.push(script.clone());
            expression_option_seen = true;
        } else if parse_options && argument.starts_with("--expression=") {
            let script = argument.trim_start_matches("--expression=");
            if script.is_empty() {
                return Err(sed_usage());
            }
            scripts.push(script.to_string());
            expression_option_seen = true;
        } else if parse_options && argument.starts_with("-e") {
            let script = &argument[2..];
            if script.is_empty() {
                return Err(sed_usage());
            }
            scripts.push(script.to_string());
            expression_option_seen = true;
        } else if parse_options && argument.starts_with('-') {
            return Err(sed_usage());
        } else if !expression_option_seen && !positional_script_seen {
            scripts.push(argument.clone());
            positional_script_seen = true;
            parse_options = false;
        } else {
            parse_options = false;
            paths.push(argument.clone());
        }
        index += 1;
    }
    if scripts.is_empty() {
        return Err(sed_usage());
    }
    Ok((suppress_default, scripts, paths))
}

fn sed_usage() -> CommandOutput {
    usage(
        "sed",
        "usage: sed [-n] [-e 's/PATTERN/REPLACEMENT/[gp]'] ... [file ...]",
    )
}

fn parse_substitution(script: &str) -> Result<Substitution, String> {
    let mut characters = script.chars();
    if characters.next() != Some('s') {
        return Err("only s/// substitution scripts are supported".to_string());
    }
    let delimiter = characters
        .next()
        .ok_or_else(|| "substitution is missing its delimiter".to_string())?;
    let pattern_text = read_script_section(&mut characters, delimiter)?;
    if pattern_text.is_empty() {
        return Err("substitution pattern must not be empty".to_string());
    }
    if pattern_text.len() > MAX_GREP_PATTERN_BYTES {
        return Err(format!(
            "substitution pattern exceeds the {MAX_GREP_PATTERN_BYTES}-byte limit"
        ));
    }
    let pattern = Regex::new(&pattern_text)
        .map_err(|error| format!("invalid regular expression: {error}"))?;
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
            if character == delimiter || character == '\\' {
                section.push(character);
            } else {
                section.push('\\');
                section.push(character);
            }
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
    let mut output = String::new();
    let mut last_end = 0;
    let mut matched = false;
    for (index, captures) in script.pattern.captures_iter(line).enumerate() {
        if !script.global && index > 0 {
            break;
        }
        let Some(full_match) = captures.get(0) else {
            continue;
        };
        output.push_str(&line[last_end..full_match.start()]);
        output.push_str(&replacement_text(&script.replacement, &captures));
        last_end = full_match.end();
        matched = true;
    }
    if matched {
        output.push_str(&line[last_end..]);
        (output, true)
    } else {
        (line.to_string(), false)
    }
}

fn replacement_text(replacement: &str, captures: &regex::Captures<'_>) -> String {
    let characters = replacement.chars().collect::<Vec<_>>();
    let mut output = String::new();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        let (escaped, next_index) = if character == '\\' || character == '$' {
            (true, index + 1)
        } else {
            (false, index)
        };
        if escaped && next_index < characters.len() && characters[next_index].is_ascii_digit() {
            let group = characters[next_index].to_digit(10).unwrap_or_default() as usize;
            if let Some(value) = captures.get(group) {
                output.push_str(value.as_str());
            }
            index = next_index + 1;
        } else if character == '&' {
            if let Some(value) = captures.get(0) {
                output.push_str(value.as_str());
            }
            index += 1;
        } else if character == '\\' && next_index < characters.len() {
            output.push(characters[next_index]);
            index = next_index + 1;
        } else if character == '$' && next_index < characters.len() {
            output.push('$');
            index += 1;
        } else {
            output.push(character);
            index += 1;
        }
    }
    output
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

#[derive(Clone, Copy)]
enum LineSelection {
    First(usize),
    Last(usize),
    From(usize),
    WithoutLast(usize),
}

fn parse_count(
    command: &str,
    args: &[String],
) -> Result<(LineSelection, Vec<String>), CommandOutput> {
    let default_selection = if command == "head" {
        LineSelection::First(10)
    } else {
        LineSelection::Last(10)
    };
    let mut selection = default_selection;
    let mut paths = Vec::new();
    let mut index = 0;
    let usage_message = format!("usage: {command} [-n [+|-]NUMBER] [--] [file ...]");
    while index < args.len() {
        let argument = &args[index];
        if argument == "--" {
            paths.extend(args[index + 1..].iter().cloned());
            break;
        } else if matches!(argument.as_str(), "-n" | "--lines") {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err(usage(command, &usage_message));
            };
            selection = parse_line_selection(command, value, default_selection)?;
        } else if let Some(value) = argument.strip_prefix("--lines=") {
            selection = parse_line_selection(command, value, default_selection)?;
        } else if let Some(value) = argument.strip_prefix("-n") {
            selection = parse_line_selection(command, value, default_selection)?;
        } else if argument.len() > 1
            && argument.starts_with('-')
            && argument[1..]
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            selection = parse_line_selection(command, &argument[1..], default_selection)?;
        } else if argument.starts_with('-') {
            return Err(usage(command, &usage_message));
        } else {
            paths.push(argument.clone());
        }
        index += 1;
    }
    Ok((selection, paths))
}

fn parse_line_selection(
    command: &str,
    value: &str,
    default_selection: LineSelection,
) -> Result<LineSelection, CommandOutput> {
    let (sign, digits) = match value.strip_prefix(['+', '-']) {
        Some(digits) => (value.as_bytes().first().copied(), digits),
        None => (None, value),
    };
    if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
        return Err(usage(
            command,
            "-n requires a signed non-negative decimal number",
        ));
    }
    let count = digits
        .parse::<usize>()
        .map_err(|_| usage(command, "-n requires a signed non-negative decimal number"))?;
    Ok(match sign {
        Some(b'+') => LineSelection::From(count.max(1)),
        Some(b'-') if matches!(default_selection, LineSelection::First(_)) => {
            LineSelection::WithoutLast(count)
        }
        Some(b'-') => LineSelection::Last(count),
        None => match default_selection {
            LineSelection::First(_) => LineSelection::First(count),
            LineSelection::Last(_) => LineSelection::Last(count),
            LineSelection::From(_) | LineSelection::WithoutLast(_) => unreachable!(),
        },
        Some(_) => unreachable!(),
    })
}

fn select_lines(text: &str, selection: LineSelection) -> String {
    let lines = lines_with_endings(text);
    let selected = match selection {
        LineSelection::First(count) => &lines[..lines.len().min(count)],
        LineSelection::Last(count) => &lines[lines.len().saturating_sub(count)..],
        LineSelection::From(start) => {
            let start = start.saturating_sub(1).min(lines.len());
            &lines[start..]
        }
        LineSelection::WithoutLast(count) => &lines[..lines.len().saturating_sub(count)],
    };
    selected.concat()
}

fn parse_grep_args(
    args: &[String],
    default_mode: GrepMode,
    command: &str,
) -> Result<GrepOptions, CommandOutput> {
    let mut case = GrepCase::Sensitive;
    let mut matching = GrepMatch::Contains;
    let mut output = GrepOutput::Lines;
    let mut mode = default_mode;
    let mut explicit_pattern = None;
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument == "--" {
            index += 1;
            break;
        }
        if argument == "-e" || argument == "--regexp" {
            index += 1;
            let Some(pattern) = args.get(index) else {
                return Err(grep_usage(command));
            };
            if explicit_pattern.replace(pattern.clone()).is_some() {
                return Err(grep_usage(command));
            }
            index += 1;
            continue;
        }
        if argument == "-E" || argument == "--extended-regexp" {
            mode = GrepMode::Regex;
            index += 1;
            continue;
        }
        if argument == "-F" || argument == "--fixed-strings" {
            mode = GrepMode::Fixed;
            index += 1;
            continue;
        }
        if argument == "-i" || argument == "--ignore-case" {
            case = GrepCase::Insensitive;
            index += 1;
            continue;
        }
        if argument == "-v" || argument == "--invert-match" {
            matching = GrepMatch::Excludes;
            index += 1;
            continue;
        }
        if argument == "-n" || argument == "--line-number" {
            output = GrepOutput::NumberedLines;
            index += 1;
            continue;
        }
        if argument == "-c" || argument == "--count" {
            output = GrepOutput::Count;
            index += 1;
            continue;
        }
        if argument.starts_with('-') && argument != "-" {
            let flags = argument.strip_prefix('-').unwrap_or_default();
            if flags.is_empty()
                || !flags
                    .chars()
                    .all(|flag| matches!(flag, 'E' | 'F' | 'i' | 'v' | 'n' | 'c'))
            {
                return Err(grep_usage(command));
            }
            for flag in flags.chars() {
                match flag {
                    'E' => mode = GrepMode::Regex,
                    'F' => mode = GrepMode::Fixed,
                    'i' => case = GrepCase::Insensitive,
                    'v' => matching = GrepMatch::Excludes,
                    'n' => output = GrepOutput::NumberedLines,
                    'c' => output = GrepOutput::Count,
                    _ => unreachable!("grep flags were validated above"),
                }
            }
            index += 1;
            continue;
        }
        break;
    }
    let pattern_was_explicit = explicit_pattern.is_some();
    let pattern = explicit_pattern.or_else(|| args.get(index).cloned());
    let Some(pattern) = pattern else {
        return Err(grep_usage(command));
    };
    if !pattern_was_explicit {
        index += 1;
    }
    let paths = args[index..].to_vec();
    Ok(GrepOptions {
        case,
        matching,
        output,
        mode,
        pattern: pattern.clone(),
        paths,
    })
}

fn grep_usage(command: &str) -> CommandOutput {
    usage(
        command,
        "usage: grep [-E|-F] [-i] [-v] [-n] [-c] [-e pattern] pattern [file ...]",
    )
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

fn parse_wc_args(args: &[String]) -> Result<(WcOptions, Vec<String>), CommandOutput> {
    let mut options = WcOptions {
        fields: WcFields::default(),
    };
    let mut paths = Vec::new();
    let mut parse_options = true;
    for argument in args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "-" {
            paths.push(argument.clone());
        } else if parse_options {
            let long_option = match argument.as_str() {
                "--lines" => Some('l'),
                "--words" => Some('w'),
                "--bytes" => Some('c'),
                "--chars" | "--characters" => Some('m'),
                "--max-line-length" => Some('L'),
                _ => None,
            };
            if let Some(flag) = long_option {
                apply_wc_flag(&mut options, flag);
            } else if let Some(flags) = argument.strip_prefix('-') {
                if flags.is_empty()
                    || !flags
                        .chars()
                        .all(|flag| matches!(flag, 'l' | 'w' | 'c' | 'm' | 'L'))
                {
                    return Err(wc_usage());
                }
                for flag in flags.chars() {
                    apply_wc_flag(&mut options, flag);
                }
            } else {
                paths.push(argument.clone());
            }
        } else {
            paths.push(argument.clone());
        }
    }
    if options.fields.0 == 0 {
        options.fields.insert(WcFields::LINES);
        options.fields.insert(WcFields::WORDS);
        options.fields.insert(WcFields::BYTES);
    }
    Ok((options, paths))
}

fn apply_wc_flag(options: &mut WcOptions, flag: char) {
    match flag {
        'l' => options.fields.insert(WcFields::LINES),
        'w' => options.fields.insert(WcFields::WORDS),
        'c' => {
            options.fields.insert(WcFields::BYTES);
            options.fields.remove(WcFields::CHARACTERS);
        }
        'm' => {
            options.fields.insert(WcFields::CHARACTERS);
            options.fields.remove(WcFields::BYTES);
        }
        'L' => options.fields.insert(WcFields::MAX_LINE_LENGTH),
        _ => unreachable!("wc flags were validated before application"),
    }
}

fn wc_usage() -> CommandOutput {
    usage(
        "wc",
        "usage: wc [-lwcLm] [--lines|--words|--bytes|--chars|--max-line-length] [--] [file ...]",
    )
}
