use regex::Regex;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

const MAX_AWK_PROGRAM_BYTES: usize = 16 * 1024;
const MAX_AWK_INPUT_BYTES: usize = 1024 * 1024;
const MAX_AWK_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_AWK_LINES: usize = 10_000;
const MAX_AWK_RULES: usize = 64;
const MAX_AWK_STATEMENTS: usize = 64;
const MAX_AWK_REGEX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
struct Rule {
    pattern: Pattern,
    statements: Vec<String>,
}

#[derive(Debug, Clone)]
enum Pattern {
    Always,
    Begin,
    End,
    Regex(Regex),
    Comparison {
        value: ValueRef,
        operator: Comparison,
        right: PatternValue,
    },
}

#[derive(Debug, Clone)]
enum PatternValue {
    Literal(String),
    Regex(Regex),
}

#[derive(Debug, Clone, Copy)]
enum Comparison {
    Equal,
    NotEqual,
    Contains,
    DoesNotContain,
}

#[derive(Debug, Clone, Copy)]
enum ValueRef {
    Record,
    Field(usize),
    LastField,
    FieldCount,
    RecordNumber,
    FileRecordNumber,
}

struct Record {
    line: String,
    fields: Vec<String>,
    number: usize,
    file_number: usize,
}

struct Runtime {
    field_separator: String,
    output_separator: String,
    output: String,
}

#[derive(Clone, Copy)]
enum ExecutionPhase {
    Begin,
    Record,
    End,
}

pub(super) fn awk(context: &mut CommandContext<'_>) -> CommandOutput {
    let (field_separator, program, paths) = match parse_arguments(context.args) {
        Ok(arguments) => arguments,
        Err(output) => return output,
    };
    if program.len() > MAX_AWK_PROGRAM_BYTES {
        return awk_failure("program exceeds the 16 KiB limit");
    }
    let rules = match parse_program(&program) {
        Ok(rules) => rules,
        Err(error) => return awk_failure(&error),
    };
    let mut runtime = Runtime {
        field_separator,
        output_separator: " ".to_string(),
        output: String::new(),
    };

    if let Err(error) = execute_rules(&rules, &mut runtime, ExecutionPhase::Begin, None) {
        return awk_failure(&error);
    }

    let inputs = if paths.is_empty() {
        vec![(None, context.stdin.to_string())]
    } else {
        let mut inputs = Vec::with_capacity(paths.len());
        for path in paths {
            if path == "-" {
                inputs.push((None, context.stdin.to_string()));
                continue;
            }
            match context.fs.metadata(&path) {
                Ok(info) if info.size > MAX_AWK_INPUT_BYTES as u64 => {
                    return awk_failure(&format!(
                        "{path}: input exceeds the {} MiB limit",
                        MAX_AWK_INPUT_BYTES / (1024 * 1024)
                    ));
                }
                Ok(_) => {}
                Err(error) => return fs_failure("awk", &error),
            }
            let bytes = match context.fs.read(&path) {
                Ok(bytes) => bytes,
                Err(error) => return fs_failure("awk", &error),
            };
            let Ok(text) = String::from_utf8(bytes) else {
                return awk_failure(&format!("{path}: input is not valid UTF-8"));
            };
            inputs.push((Some(path), text));
        }
        inputs
    };

    let mut record_number = 0;
    for (_path, text) in inputs {
        for (line_index, line) in text.lines().enumerate() {
            record_number += 1;
            let file_record_number = line_index + 1;
            if record_number > MAX_AWK_LINES {
                return awk_failure("input exceeds the 10,000-line limit");
            }
            if let Some(cancelled) = context.take_cancellation() {
                return cancelled;
            }
            let fields = split_fields(line, &runtime.field_separator);
            let record = Record {
                line: line.to_string(),
                fields,
                number: record_number,
                file_number: file_record_number,
            };
            if let Err(error) =
                execute_rules(&rules, &mut runtime, ExecutionPhase::Record, Some(&record))
            {
                return awk_failure(&error);
            }
        }
    }
    if let Err(error) = execute_rules(&rules, &mut runtime, ExecutionPhase::End, None) {
        return awk_failure(&error);
    }
    CommandOutput::success(runtime.output)
}

fn parse_arguments(args: &[String]) -> Result<(String, String, Vec<String>), CommandOutput> {
    let mut field_separator = " ".to_string();
    let mut program = None;
    let mut paths = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if program.is_none() && argument == "--" {
            index += 1;
            continue;
        }
        if program.is_none() && argument == "-F" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err(usage("awk", "usage: awk [-F CHAR] PROGRAM [FILE ...]"));
            };
            field_separator = parse_field_separator(value)?;
        } else if program.is_none() && argument.starts_with("-F") {
            field_separator = parse_field_separator(&argument[2..])?;
        } else if program.is_none() && argument.starts_with('-') {
            return Err(usage("awk", "usage: awk [-F CHAR] PROGRAM [FILE ...]"));
        } else if program.is_some() {
            paths.push(argument.clone());
        } else {
            program = Some(argument.clone());
        }
        index += 1;
    }
    let Some(program) = program else {
        return Err(usage("awk", "usage: awk [-F CHAR] PROGRAM [FILE ...]"));
    };
    Ok((field_separator, program, paths))
}

fn parse_field_separator(value: &str) -> Result<String, CommandOutput> {
    if value == "\\t" {
        return Ok("\t".to_string());
    }
    let mut characters = value.chars();
    let Some(character) = characters.next() else {
        return Err(usage("awk", "-F requires one character or \\t"));
    };
    if characters.next().is_some() {
        return Err(usage("awk", "-F requires one character or \\t"));
    }
    Ok(character.to_string())
}

fn parse_program(program: &str) -> Result<Vec<Rule>, String> {
    let mut rules = Vec::new();
    let mut cursor = 0;
    while cursor < program.len() {
        while cursor < program.len()
            && program[cursor..]
                .chars()
                .next()
                .is_some_and(|character| character.is_whitespace() || character == ';')
        {
            cursor += program[cursor..].chars().next().map_or(1, char::len_utf8);
        }
        if cursor == program.len() {
            break;
        }
        let Some(open) = find_unquoted(program, cursor, '{') else {
            return Err("program requires rule actions in braces".to_string());
        };
        let Some(close) = matching_brace(program, open) else {
            return Err("program has an unclosed action block".to_string());
        };
        let pattern = parse_pattern(program[cursor..open].trim())?;
        let statements = split_statements(&program[open + 1..close])?;
        if statements.len() > MAX_AWK_STATEMENTS {
            return Err("action contains too many statements".to_string());
        }
        rules.push(Rule {
            pattern,
            statements,
        });
        if rules.len() > MAX_AWK_RULES {
            return Err("program contains too many rules".to_string());
        }
        cursor = close + 1;
    }
    if rules.is_empty() {
        return Err("program must contain at least one rule".to_string());
    }
    Ok(rules)
}

fn parse_pattern(pattern: &str) -> Result<Pattern, String> {
    if pattern.is_empty() {
        return Ok(Pattern::Always);
    }
    if pattern == "BEGIN" {
        return Ok(Pattern::Begin);
    }
    if pattern == "END" {
        return Ok(Pattern::End);
    }
    if pattern.starts_with('/') && pattern.ends_with('/') && pattern.len() >= 2 {
        return Ok(Pattern::Regex(parse_regex_literal(pattern)?));
    }
    for (operator_text, operator) in [
        ("!~", Comparison::DoesNotContain),
        ("==", Comparison::Equal),
        ("!=", Comparison::NotEqual),
        ("~", Comparison::Contains),
    ] {
        if let Some(index) = pattern.find(operator_text) {
            let left = parse_value_ref(pattern[..index].trim())?;
            let right = parse_pattern_value(
                pattern[index + operator_text.len()..].trim(),
                matches!(operator, Comparison::Contains | Comparison::DoesNotContain),
            )?;
            return Ok(Pattern::Comparison {
                value: left,
                operator,
                right,
            });
        }
    }
    Err("unsupported pattern; use /regex/, FIELD == VALUE, or FIELD ~ /regex/".to_string())
}

fn parse_pattern_value(value: &str, regex_allowed: bool) -> Result<PatternValue, String> {
    if regex_allowed && value.starts_with('/') && value.ends_with('/') && value.len() >= 2 {
        return Ok(PatternValue::Regex(parse_regex_literal(value)?));
    }
    if value.starts_with('/') && value.ends_with('/') && value.len() >= 2 {
        return Ok(PatternValue::Literal(value[1..value.len() - 1].to_string()));
    }
    parse_literal(value).map(PatternValue::Literal)
}

fn parse_regex_literal(value: &str) -> Result<Regex, String> {
    let body = &value[1..value.len() - 1];
    if body.len() > MAX_AWK_REGEX_BYTES {
        return Err(format!(
            "regular expression exceeds the {MAX_AWK_REGEX_BYTES}-byte limit"
        ));
    }
    let mut pattern = String::with_capacity(body.len());
    let mut escaped = false;
    for character in body.chars() {
        if escaped {
            if character == '/' {
                pattern.push('/');
            } else {
                pattern.push('\\');
                pattern.push(character);
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            pattern.push(character);
        }
    }
    if escaped {
        pattern.push('\\');
    }
    Regex::new(&pattern).map_err(|error| format!("invalid regular expression: {error}"))
}

fn parse_value_ref(value: &str) -> Result<ValueRef, String> {
    match value {
        "$0" => Ok(ValueRef::Record),
        "$NF" => Ok(ValueRef::LastField),
        "NF" => Ok(ValueRef::FieldCount),
        "NR" => Ok(ValueRef::RecordNumber),
        "FNR" => Ok(ValueRef::FileRecordNumber),
        _ => {
            let Some(index) = value.strip_prefix('$') else {
                return Err(format!("unsupported value reference: {value}"));
            };
            let Ok(index) = index.parse::<usize>() else {
                return Err(format!("unsupported field reference: {value}"));
            };
            Ok(ValueRef::Field(index))
        }
    }
}

fn split_statements(action: &str) -> Result<Vec<String>, String> {
    let mut statements = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in action.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if character == '"' {
            quote = if quote.is_some() {
                None
            } else {
                Some(character)
            };
        } else if quote.is_none() && matches!(character, ';' | '\n') {
            let statement = action[start..index].trim();
            if !statement.is_empty() {
                statements.push(statement.to_string());
            }
            start = index + character.len_utf8();
        }
    }
    if quote.is_some() || escaped {
        return Err("action contains an unclosed string".to_string());
    }
    let statement = action[start..].trim();
    if !statement.is_empty() {
        statements.push(statement.to_string());
    }
    Ok(statements)
}

fn execute_rules(
    rules: &[Rule],
    runtime: &mut Runtime,
    phase: ExecutionPhase,
    record: Option<&Record>,
) -> Result<(), String> {
    for rule in rules {
        let matches = match (&rule.pattern, phase, record) {
            (Pattern::Begin, ExecutionPhase::Begin, None)
            | (Pattern::End, ExecutionPhase::End, None) => true,
            (Pattern::Begin | Pattern::End, _, _) => false,
            (pattern, ExecutionPhase::Record, record) => pattern_matches(pattern, record),
            (Pattern::Always | Pattern::Comparison { .. } | Pattern::Regex(_), _, _) => false,
        };
        if matches {
            execute_statements(&rule.statements, runtime, record)?;
        }
    }
    Ok(())
}

fn pattern_matches(pattern: &Pattern, record: Option<&Record>) -> bool {
    let Some(record) = record else { return false };
    match pattern {
        Pattern::Always => true,
        Pattern::Begin | Pattern::End => false,
        Pattern::Regex(regex) => regex.is_match(&record.line),
        Pattern::Comparison {
            value,
            operator,
            right,
        } => {
            let left = value_of(*value, record);
            match operator {
                Comparison::Equal => {
                    matches!(right, PatternValue::Literal(value) if left == *value)
                }
                Comparison::NotEqual => {
                    !matches!(right, PatternValue::Literal(value) if left == *value)
                }
                Comparison::Contains => matches_pattern_value(&left, right),
                Comparison::DoesNotContain => !matches_pattern_value(&left, right),
            }
        }
    }
}

fn matches_pattern_value(value: &str, pattern: &PatternValue) -> bool {
    match pattern {
        PatternValue::Literal(pattern) => value.contains(pattern),
        PatternValue::Regex(regex) => regex.is_match(value),
    }
}

fn execute_statements(
    statements: &[String],
    runtime: &mut Runtime,
    record: Option<&Record>,
) -> Result<(), String> {
    for statement in statements {
        if statement == "next" {
            return Err("next is not supported in the bounded subset".to_string());
        }
        if let Some((name, value)) = statement.split_once('=') {
            match name.trim() {
                "OFS" => {
                    runtime.output_separator = parse_literal(value.trim())?;
                    continue;
                }
                "FS" => {
                    runtime.field_separator = parse_field_separator_literal(value.trim())?;
                    continue;
                }
                _ => {}
            }
        }
        if statement == "print" {
            append_output(
                runtime,
                &value_of(ValueRef::Record, record_or_empty(record)),
            )?;
            append_output(runtime, "\n")?;
            continue;
        }
        let Some(expressions) = statement.strip_prefix("print ") else {
            return Err(format!(
                "unsupported action: {statement}; only print and FS/OFS assignment are supported"
            ));
        };
        let expressions = split_expressions(expressions)?;
        if expressions.is_empty() {
            return Err("print requires an expression".to_string());
        }
        for (index, expression) in expressions.iter().enumerate() {
            if index > 0 {
                let separator = runtime.output_separator.clone();
                append_output(runtime, &separator)?;
            }
            let value = evaluate_expression(expression, runtime, record)?;
            append_output(runtime, &value)?;
        }
        append_output(runtime, "\n")?;
    }
    Ok(())
}

fn split_expressions(expressions: &str) -> Result<Vec<String>, String> {
    let mut values = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in expressions.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if character == '"' {
            quote = if quote.is_some() {
                None
            } else {
                Some(character)
            };
        } else if character == ',' && quote.is_none() {
            let value = expressions[start..index].trim();
            if value.is_empty() {
                return Err("print contains an empty expression".to_string());
            }
            values.push(value.to_string());
            start = index + 1;
        }
    }
    if quote.is_some() || escaped {
        return Err("print contains an unclosed string".to_string());
    }
    let value = expressions[start..].trim();
    if value.is_empty() {
        return Err("print contains an empty expression".to_string());
    }
    values.push(value.to_string());
    Ok(values)
}

fn evaluate_expression(
    expression: &str,
    runtime: &Runtime,
    record: Option<&Record>,
) -> Result<String, String> {
    let expression = expression.trim();
    if expression.starts_with("length(") && expression.ends_with(')') {
        let inner = &expression[7..expression.len() - 1];
        return Ok(evaluate_expression(inner, runtime, record)?
            .chars()
            .count()
            .to_string());
    }
    if expression.starts_with("tolower(") && expression.ends_with(')') {
        let inner = &expression[8..expression.len() - 1];
        return Ok(evaluate_expression(inner, runtime, record)?.to_lowercase());
    }
    if expression.starts_with("toupper(") && expression.ends_with(')') {
        let inner = &expression[8..expression.len() - 1];
        return Ok(evaluate_expression(inner, runtime, record)?.to_uppercase());
    }
    match expression {
        "FS" => Ok(runtime.field_separator.clone()),
        "OFS" => Ok(runtime.output_separator.clone()),
        "NF" | "NR" | "FNR" | "$0" | "$NF" => Ok(value_of(
            parse_value_ref(expression)?,
            record_or_empty(record),
        )),
        _ if expression.starts_with('$') => Ok(value_of(
            parse_value_ref(expression)?,
            record_or_empty(record),
        )),
        _ => parse_literal(expression),
    }
}

fn parse_literal(value: &str) -> Result<String, String> {
    if value.starts_with('"') {
        if !value.ends_with('"') || value.len() < 2 {
            return Err("unterminated string literal".to_string());
        }
        return Ok(unescape(&value[1..value.len() - 1]));
    }
    if value.is_empty() {
        return Err("empty literal".to_string());
    }
    if value.chars().any(char::is_whitespace) {
        return Err(format!("unsupported literal: {value}"));
    }
    Ok(value.to_string())
}

fn parse_field_separator_literal(value: &str) -> Result<String, String> {
    if value == "\"\\t\"" {
        return Ok("\t".to_string());
    }
    parse_literal(value).and_then(|value| {
        if value == " " || value.chars().count() == 1 {
            Ok(value)
        } else {
            Err("FS must be one character or a single space".to_string())
        }
    })
}

fn unescape(value: &str) -> String {
    let mut output = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            output.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            output.push(character);
        }
    }
    if escaped {
        output.push('\\');
    }
    output
}

fn split_fields(line: &str, separator: &str) -> Vec<String> {
    if separator == " " || separator.is_empty() {
        line.split_whitespace().map(str::to_string).collect()
    } else {
        line.split(separator).map(str::to_string).collect()
    }
}

fn value_of(value: ValueRef, record: &Record) -> String {
    match value {
        ValueRef::Record | ValueRef::Field(0) => record.line.clone(),
        ValueRef::Field(index) => record.fields.get(index - 1).cloned().unwrap_or_default(),
        ValueRef::LastField => record.fields.last().cloned().unwrap_or_default(),
        ValueRef::FieldCount => record.fields.len().to_string(),
        ValueRef::RecordNumber => record.number.to_string(),
        ValueRef::FileRecordNumber => record.file_number.to_string(),
    }
}

fn record_or_empty(record: Option<&Record>) -> &Record {
    record.unwrap_or(&EMPTY_RECORD)
}

static EMPTY_RECORD: Record = Record {
    line: String::new(),
    fields: Vec::new(),
    number: 0,
    file_number: 0,
};

fn append_output(runtime: &mut Runtime, value: &str) -> Result<(), String> {
    if runtime.output.len().saturating_add(value.len()) > MAX_AWK_OUTPUT_BYTES {
        return Err("output exceeds the 1 MiB limit".to_string());
    }
    runtime.output.push_str(value);
    Ok(())
}

fn find_unquoted(value: &str, start: usize, needle: char) -> Option<usize> {
    let mut quote = false;
    let mut escaped = false;
    for (index, character) in value[start..].char_indices() {
        let index = start + index;
        if escaped {
            escaped = false;
        } else if character == '\\' && quote {
            escaped = true;
        } else if character == '"' {
            quote = !quote;
        } else if character == needle && !quote {
            return Some(index);
        }
    }
    None
}

fn matching_brace(value: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut quote = false;
    let mut escaped = false;
    for (offset, character) in value[open..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote {
            escaped = true;
            continue;
        }
        if character == '"' {
            quote = !quote;
        } else if !quote && character == '{' {
            depth += 1;
        } else if !quote && character == '}' {
            depth -= 1;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

fn awk_failure(message: &str) -> CommandOutput {
    CommandOutput::failure(2, format!("awk: {message}\n"))
}
