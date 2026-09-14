use std::fmt::Write as _;

use chrono::{DateTime, Local, Utc};

use crate::{fs_failure, usage, CommandContext, CommandOutput};
use rune_package::sha256_hex;

const MAX_HEXDUMP_INPUT: usize = 256 * 1024;
const MAX_BASE64_INPUT: usize = 768 * 1024;
const MAX_CKSUM_INPUT: usize = 16 * 1024 * 1024;
const MAX_MD5_INPUT: usize = 16 * 1024 * 1024;
const MAX_SUM_INPUT: usize = 16 * 1024 * 1024;
const MAX_BC_SOURCE_BYTES: usize = 256 * 1024;
const MAX_BC_STATEMENTS: usize = 1_024;
const MAX_BC_TOKENS: usize = 8_192;
const MAX_BC_PARENTHESIS_DEPTH: usize = 64;
const MAX_DATE_FORMAT_BYTES: usize = 1024;
const MAX_DISK_USAGE_ENTRIES: usize = 10_000;
const MAX_EXPR_ARGUMENTS: usize = 64;
const MAX_EXPR_TEXT_BYTES: usize = 64 * 1024;
const MAX_MKTEMP_ATTEMPTS: usize = 128;
const MAX_MKTEMP_TEMPLATES: usize = 64;
const MAX_MKTEMP_TEMPLATE_BYTES: usize = 1024;
const MIN_MKTEMP_X_COUNT: usize = 3;
const MAX_MKTEMP_X_COUNT: usize = 32;
const MAX_FILE_PROBE_BYTES: usize = 256 * 1024;
const MAX_SEQ_VALUES: usize = 100_000;

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const MD5_SHIFTS: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const MD5_CONSTANTS: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

pub(super) fn base64(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut decode = false;
    let mut path = None;
    let mut parse_options = true;
    for argument in context.args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && matches!(argument.as_str(), "-d" | "-D" | "--decode") {
            decode = true;
        } else if (parse_options && argument.starts_with('-')) || path.is_some() {
            return usage("base64", "usage: base64 [-d|--decode] [--] [FILE]");
        } else {
            path = Some(argument.as_str());
        }
    }

    let path = path.unwrap_or("-");
    let bytes = if path == "-" {
        context.stdin.as_bytes().to_vec()
    } else {
        match context.fs.read(path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure("base64", &error),
        }
    };
    if bytes.len() > MAX_BASE64_INPUT {
        return CommandOutput::failure(
            1,
            format!("base64: input exceeds {MAX_BASE64_INPUT} bytes\n"),
        );
    }

    if decode {
        let decoded = match decode_base64(&bytes) {
            Ok(decoded) => decoded,
            Err(message) => return CommandOutput::failure(1, format!("base64: {message}\n")),
        };
        return match String::from_utf8(decoded) {
            Ok(text) => CommandOutput::success(text),
            Err(_) => CommandOutput::failure(1, "base64: decoded output is not valid UTF-8\n"),
        };
    }

    CommandOutput::success(encode_base64(&bytes))
}

pub(super) fn bc(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut path = None;
    let mut parse_options = true;
    for argument in context.args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "-q" {
            // Rune has no startup banner for this command, so quiet is a
            // harmless compatibility spelling.
        } else if parse_options && argument == "-l" {
            return CommandOutput::failure(
                2,
                "bc: the standard math library is unavailable in the bounded provider\n",
            );
        } else if (parse_options && argument.starts_with('-')) || path.is_some() {
            return usage("bc", "usage: bc [-q] [--] [FILE]");
        } else {
            path = Some(argument.as_str());
        }
    }

    let source = match path {
        Some(path) => match context.fs.read(path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure("bc", &error),
        },
        None => context.stdin.as_bytes().to_vec(),
    };
    if source.len() > MAX_BC_SOURCE_BYTES {
        return CommandOutput::failure(
            1,
            format!("bc: input exceeds {MAX_BC_SOURCE_BYTES} bytes\n"),
        );
    }
    let Ok(source) = std::str::from_utf8(&source) else {
        return CommandOutput::failure(1, "bc: input is not valid UTF-8\n");
    };
    let values = match BcParser::new(source).parse_program() {
        Ok(values) => values,
        Err(error) => return CommandOutput::failure(1, format!("bc: {error}\n")),
    };
    let mut stdout = String::new();
    for value in values {
        let _ = writeln!(stdout, "{value}");
    }
    CommandOutput::success(stdout)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BcToken {
    Integer(i64),
    Plus,
    Minus,
    Multiply,
    Divide,
    Remainder,
    Power,
    OpenParen,
    CloseParen,
    Newline,
    Semicolon,
    Invalid(char),
    InvalidLiteral,
    TokenLimit,
    End,
}

struct BcParser {
    tokens: Vec<BcToken>,
    position: usize,
    parenthesis_depth: usize,
}

impl BcParser {
    fn new(source: &str) -> Self {
        Self {
            tokens: tokenize_bc(source),
            position: 0,
            parenthesis_depth: 0,
        }
    }

    fn parse_program(&mut self) -> Result<Vec<i64>, String> {
        let mut values = Vec::new();
        loop {
            self.skip_separators();
            if self.current() == BcToken::End {
                return Ok(values);
            }
            if let BcToken::TokenLimit = self.current() {
                return Err(format!("input exceeds {MAX_BC_TOKENS} tokens"));
            }
            if let BcToken::Invalid(character) = self.current() {
                return Err(format!("unsupported character: {character}"));
            }
            if self.current() == BcToken::InvalidLiteral {
                return Err("integer literal is out of range".to_string());
            }
            if values.len() >= MAX_BC_STATEMENTS {
                return Err(format!("input exceeds {MAX_BC_STATEMENTS} statements"));
            }
            let value = self.parse_additive()?;
            match self.current() {
                BcToken::End | BcToken::Newline | BcToken::Semicolon => values.push(value),
                _ => return Err("expected a statement separator".to_string()),
            }
        }
    }

    fn parse_additive(&mut self) -> Result<i64, String> {
        let mut value = self.parse_multiplicative()?;
        loop {
            let operator = match self.current() {
                BcToken::Plus => "+",
                BcToken::Minus => "-",
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            value = apply_bc_arithmetic(value, right, operator)?;
        }
        Ok(value)
    }

    fn parse_multiplicative(&mut self) -> Result<i64, String> {
        let mut value = self.parse_power()?;
        loop {
            let operator = match self.current() {
                BcToken::Multiply => "*",
                BcToken::Divide => "/",
                BcToken::Remainder => "%",
                _ => break,
            };
            self.advance();
            let right = self.parse_power()?;
            value = apply_bc_arithmetic(value, right, operator)?;
        }
        Ok(value)
    }

    fn parse_power(&mut self) -> Result<i64, String> {
        let value = self.parse_unary()?;
        if self.current() != BcToken::Power {
            return Ok(value);
        }
        self.advance();
        let exponent = self.parse_power()?;
        if exponent < 0 {
            return Err("negative exponents are unavailable".to_string());
        }
        let exponent = u32::try_from(exponent)
            .map_err(|_| "exponent exceeds the supported range".to_string())?;
        value
            .checked_pow(exponent)
            .ok_or_else(|| "integer overflow for operator ^".to_string())
    }

    fn parse_unary(&mut self) -> Result<i64, String> {
        match self.current() {
            BcToken::Plus => {
                self.advance();
                self.parse_unary()
            }
            BcToken::Minus => {
                self.advance();
                self.parse_unary()?
                    .checked_neg()
                    .ok_or_else(|| "integer overflow for unary -".to_string())
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<i64, String> {
        match self.advance() {
            BcToken::Integer(value) => Ok(value),
            BcToken::OpenParen => {
                self.parenthesis_depth += 1;
                if self.parenthesis_depth > MAX_BC_PARENTHESIS_DEPTH {
                    return Err(format!(
                        "parenthesis depth exceeds {MAX_BC_PARENTHESIS_DEPTH}"
                    ));
                }
                let value = self.parse_additive()?;
                if self.advance() != BcToken::CloseParen {
                    return Err("missing closing parenthesis".to_string());
                }
                self.parenthesis_depth -= 1;
                Ok(value)
            }
            BcToken::End | BcToken::Newline | BcToken::Semicolon => {
                Err("missing operand".to_string())
            }
            BcToken::Invalid(character) => Err(format!("unsupported character: {character}")),
            BcToken::InvalidLiteral => Err("integer literal is out of range".to_string()),
            BcToken::TokenLimit => Err(format!("input exceeds {MAX_BC_TOKENS} tokens")),
            _ => Err("expected an integer or opening parenthesis".to_string()),
        }
    }

    fn current(&self) -> BcToken {
        self.tokens
            .get(self.position)
            .copied()
            .unwrap_or(BcToken::End)
    }

    fn advance(&mut self) -> BcToken {
        let token = self.current();
        self.position = self.position.saturating_add(1);
        token
    }

    fn skip_separators(&mut self) {
        while matches!(self.current(), BcToken::Newline | BcToken::Semicolon) {
            self.advance();
        }
    }
}

fn tokenize_bc(source: &str) -> Vec<BcToken> {
    let mut tokens = Vec::new();
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        if tokens.len() >= MAX_BC_TOKENS {
            tokens.push(BcToken::TokenLimit);
            break;
        }
        let token = match character {
            '0'..='9' => {
                let mut literal = String::from(character);
                while let Some(next @ '0'..='9') = characters.peek().copied() {
                    literal.push(next);
                    characters.next();
                }
                literal
                    .parse::<i64>()
                    .map_or(BcToken::InvalidLiteral, BcToken::Integer)
            }
            '+' => BcToken::Plus,
            '-' => BcToken::Minus,
            '*' => BcToken::Multiply,
            '/' => BcToken::Divide,
            '%' => BcToken::Remainder,
            '^' => BcToken::Power,
            '(' => BcToken::OpenParen,
            ')' => BcToken::CloseParen,
            ';' => BcToken::Semicolon,
            '\n' => BcToken::Newline,
            '\r' => continue,
            '#' => {
                for next in characters.by_ref() {
                    if next == '\n' {
                        tokens.push(BcToken::Newline);
                        break;
                    }
                }
                continue;
            }
            character if character.is_ascii_whitespace() => continue,
            _ => BcToken::Invalid(character),
        };
        tokens.push(token);
    }
    tokens.push(BcToken::End);
    tokens
}

fn apply_bc_arithmetic(left: i64, right: i64, operator: &str) -> Result<i64, String> {
    match operator {
        "+" => left.checked_add(right),
        "-" => left.checked_sub(right),
        "*" => left.checked_mul(right),
        "/" => left.checked_div(right),
        "%" => left.checked_rem(right),
        _ => None,
    }
    .ok_or_else(|| {
        if matches!(operator, "/" | "%") && right == 0 {
            "division by zero".to_string()
        } else {
            format!("integer overflow for operator {operator}")
        }
    })
}

pub(super) fn basename(context: &mut CommandContext<'_>) -> CommandOutput {
    if !(1..=2).contains(&context.args.len()) || context.args[0].is_empty() {
        return usage("basename", "usage: basename PATH [SUFFIX]");
    }
    let mut name = basename_value(&context.args[0]).to_string();
    if let Some(suffix) = context.args.get(1) {
        if !suffix.is_empty() && name != *suffix && name.ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
        }
    }
    CommandOutput::success(format!("{name}\n"))
}

pub(super) fn cksum(context: &mut CommandContext<'_>) -> CommandOutput {
    let path = match context.args {
        [] => "-",
        [path] => path.as_str(),
        [flag, path] if flag == "--" => path.as_str(),
        _ => return usage("cksum", "usage: cksum [--] [FILE]"),
    };
    let bytes = if path == "-" {
        context.stdin.as_bytes().to_vec()
    } else {
        match context.fs.read(path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure("cksum", &error),
        }
    };
    if bytes.len() > MAX_CKSUM_INPUT {
        return CommandOutput::failure(
            1,
            format!("cksum: input exceeds {MAX_CKSUM_INPUT} bytes\n"),
        );
    }
    CommandOutput::success(format!("{} {}\n", posix_cksum(&bytes), bytes.len()))
}

pub(super) fn date(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut utc = false;
    let mut format = None;
    let mut parse_options = true;
    for argument in context.args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && matches!(argument.as_str(), "-u" | "--utc") {
            utc = true;
        } else if (parse_options && argument.starts_with('-'))
            || format.is_some()
            || !argument.starts_with('+')
        {
            return usage("date", "usage: date [-u|--utc] [+FORMAT]");
        } else {
            let value = &argument[1..];
            if value.len() > MAX_DATE_FORMAT_BYTES {
                return CommandOutput::failure(
                    1,
                    format!("date: format exceeds {MAX_DATE_FORMAT_BYTES} bytes\n"),
                );
            }
            format = Some(value);
        }
    }

    let output = if utc {
        format_date(&Utc::now(), format)
    } else {
        format_date(&Local::now(), format)
    };
    CommandOutput::success(format!("{output}\n"))
}

fn format_date<T>(date: &DateTime<T>, format: Option<&str>) -> String
where
    T: chrono::TimeZone,
    T::Offset: std::fmt::Display,
{
    date.format(format.unwrap_or("%a %b %e %H:%M:%S %Z %Y"))
        .to_string()
}

pub(super) fn sum(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut system_v = false;
    let mut paths = Vec::new();
    let mut parse_options = true;
    for argument in context.args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "-s" {
            system_v = true;
        } else if parse_options && argument == "-r" {
            system_v = false;
        } else if parse_options && argument.starts_with('-') {
            return usage("sum", "usage: sum [-r|-s] [--] [FILE ...]");
        } else {
            paths.push(argument.as_str());
        }
    }
    if paths.is_empty() {
        paths.push("-");
    }

    let mut output = String::new();
    for path in paths {
        let bytes = if path == "-" {
            context.stdin.as_bytes().to_vec()
        } else {
            match context.fs.read(path) {
                Ok(bytes) => bytes,
                Err(error) => return fs_failure("sum", &error),
            }
        };
        if bytes.len() > MAX_SUM_INPUT {
            return CommandOutput::failure(
                1,
                format!("sum: input exceeds {MAX_SUM_INPUT} bytes\n"),
            );
        }
        let checksum = if system_v {
            system_v_sum(&bytes)
        } else {
            bsd_sum(&bytes)
        };
        let blocks = if system_v {
            bytes.len().saturating_add(511) / 512
        } else {
            bytes.len().saturating_add(1023) / 1024
        };
        if path == "-" && context.args.len() <= 2 {
            let _ = writeln!(output, "{checksum} {blocks}");
        } else {
            let _ = writeln!(output, "{checksum} {blocks} {path}");
        }
    }
    CommandOutput::success(output)
}

fn bsd_sum(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0_u16, |checksum, byte| {
        checksum.rotate_right(1).wrapping_add(u16::from(*byte))
    })
}

fn system_v_sum(bytes: &[u8]) -> u16 {
    let checksum = bytes
        .iter()
        .fold(0_u32, |checksum, byte| checksum + u32::from(*byte));
    u16::try_from(((checksum & 0xffff) + (checksum >> 16)) & u32::from(u16::MAX))
        .unwrap_or_default()
}

pub(super) fn mktemp(context: &mut CommandContext<'_>) -> CommandOutput {
    let (directory, quiet, prefix, mut templates) = match parse_mktemp_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    if templates.len() > MAX_MKTEMP_TEMPLATES {
        return usage(
            "mktemp",
            &format!("at most {MAX_MKTEMP_TEMPLATES} templates are supported"),
        );
    }
    if let Some(prefix) = prefix {
        if !templates.is_empty() {
            return usage("mktemp", "-t cannot be combined with an explicit template");
        }
        let Some(prefix) = valid_mktemp_prefix(&prefix) else {
            return usage(
                "mktemp",
                "-t prefix must be a non-empty filename component without controls or path separators",
            );
        };
        templates.push(format!("~/tmp/{prefix}.XXXXXXXX"));
    } else if templates.is_empty() {
        templates.push("~/tmp/rune.XXXXXXXX".to_string());
    }
    for template in &templates {
        if let Err(message) = split_mktemp_template(template) {
            return usage("mktemp", &message);
        }
    }

    let mut stdout = String::new();
    for template in templates {
        let path = match create_mktemp_path(context.fs, &template, directory) {
            Ok(path) => path,
            Err(error) => {
                if quiet {
                    return CommandOutput::failure(1, "");
                }
                return fs_failure("mktemp", &error);
            }
        };
        let _ = writeln!(stdout, "{path}");
    }
    CommandOutput::success(stdout)
}

fn parse_mktemp_args(
    args: &[String],
) -> Result<(bool, bool, Option<String>, Vec<String>), CommandOutput> {
    let mut directory = false;
    let mut quiet = false;
    let mut prefix = None;
    let mut templates = Vec::new();
    let mut parse_options = true;
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "-d" {
            directory = true;
        } else if parse_options && argument == "-q" {
            quiet = true;
        } else if parse_options && argument == "-u" {
            return Err(CommandOutput::failure(
                2,
                "mktemp: insecure name-only mode (-u) is not available\n",
            ));
        } else if parse_options && argument == "-t" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err(mktemp_usage());
            };
            prefix = Some(value.clone());
        } else if parse_options && argument.starts_with("-t") {
            let value = &argument[2..];
            if value.is_empty() {
                return Err(mktemp_usage());
            }
            prefix = Some(value.to_string());
        } else if parse_options && argument.starts_with('-') {
            return Err(mktemp_usage());
        } else {
            templates.push(argument.clone());
            parse_options = false;
        }
        index += 1;
    }
    Ok((directory, quiet, prefix, templates))
}

fn mktemp_usage() -> CommandOutput {
    usage(
        "mktemp",
        "usage: mktemp [-d] [-q] [-t prefix] [template ...]",
    )
}

fn valid_mktemp_prefix(prefix: &str) -> Option<&str> {
    (!prefix.is_empty()
        && prefix.len() <= MAX_MKTEMP_TEMPLATE_BYTES
        && prefix != "."
        && prefix != ".."
        && !prefix.contains('/')
        && !prefix.contains('\\')
        && !prefix.chars().any(char::is_control))
    .then_some(prefix)
}

fn create_mktemp_path(
    filesystem: &dyn rune_fs::VirtualFileSystem,
    template: &str,
    directory: bool,
) -> Result<String, rune_fs::FsError> {
    let (prefix, x_count, suffix) =
        split_mktemp_template(template).map_err(rune_fs::FsError::InvalidPath)?;
    for _ in 0..MAX_MKTEMP_ATTEMPTS {
        let random = secure_mktemp_suffix(x_count).map_err(|message| rune_fs::FsError::Io {
            operation: "randomize temporary name".to_string(),
            path: template.to_string(),
            message,
        })?;
        let candidate = format!("{prefix}{random}{suffix}");
        let result = if directory {
            filesystem.make_directory(&candidate, false)
        } else {
            filesystem.create_file_exclusive(&candidate)
        };
        match result {
            Ok(()) => return Ok(candidate),
            Err(rune_fs::FsError::AlreadyExists(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Err(rune_fs::FsError::Io {
        operation: "create temporary path".to_string(),
        path: template.to_string(),
        message: format!("no unused name after {MAX_MKTEMP_ATTEMPTS} attempts"),
    })
}

fn split_mktemp_template(template: &str) -> Result<(&str, usize, &str), String> {
    if template.is_empty() {
        return Err("template must not be empty".to_string());
    }
    if template.len() > MAX_MKTEMP_TEMPLATE_BYTES {
        return Err(format!(
            "template exceeds the {MAX_MKTEMP_TEMPLATE_BYTES}-byte limit"
        ));
    }
    if template.chars().any(char::is_control) {
        return Err("template must not contain control characters".to_string());
    }
    let Some(last_x) = template.rfind('X') else {
        return Err("template must contain at least three consecutive X characters".to_string());
    };
    let bytes = template.as_bytes();
    let mut start = last_x;
    while start > 0 && bytes[start - 1] == b'X' {
        start -= 1;
    }
    let end = last_x + 1;
    let x_count = end - start;
    if x_count < MIN_MKTEMP_X_COUNT {
        return Err(format!(
            "template must contain at least {MIN_MKTEMP_X_COUNT} consecutive X characters"
        ));
    }
    if x_count > MAX_MKTEMP_X_COUNT {
        return Err(format!(
            "template contains more than {MAX_MKTEMP_X_COUNT} consecutive X characters"
        ));
    }
    Ok((&template[..start], x_count, &template[end..]))
}

fn secure_mktemp_suffix(length: usize) -> Result<String, String> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut bytes = vec![0_u8; length];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("secure random source unavailable: {error}"))?;
    Ok(bytes
        .into_iter()
        .map(|byte| ALPHABET[usize::from(byte) % ALPHABET.len()] as char)
        .collect())
}

pub(super) fn md5(context: &mut CommandContext<'_>) -> CommandOutput {
    let path = match context.args {
        [] => "-",
        [path] => path.as_str(),
        [flag, path] if flag == "--" => path.as_str(),
        _ => return usage("md5", "usage: md5 [--] [FILE]"),
    };
    let bytes = if path == "-" {
        context.stdin.as_bytes().to_vec()
    } else {
        match context.fs.read(path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure("md5", &error),
        }
    };
    if bytes.len() > MAX_MD5_INPUT {
        return CommandOutput::failure(1, format!("md5: input exceeds {MAX_MD5_INPUT} bytes\n"));
    }
    CommandOutput::success(format!("{}  {path}\n", md5_hex(&bytes)))
}

pub(super) fn expr(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("expr", "usage: expr EXPRESSION");
    }
    if context.args.len() > MAX_EXPR_ARGUMENTS {
        return expr_failure(format!("expression exceeds {MAX_EXPR_ARGUMENTS} arguments"));
    }
    if context
        .args
        .iter()
        .any(|argument| argument.len() > MAX_EXPR_TEXT_BYTES)
    {
        return expr_failure(format!("text operand exceeds {MAX_EXPR_TEXT_BYTES} bytes"));
    }

    let value = match context.args.first().map(String::as_str) {
        Some("length") => expr_length(context.args),
        Some("index") => expr_index(context.args),
        Some("substr") => expr_substr(context.args),
        Some(_) => {
            let mut parser = ExprParser::new(context.args);
            let value = parser.parse_comparison();
            match value {
                Ok(value) if parser.is_at_end() => Ok(value),
                Ok(_) => Err("unexpected operand".to_string()),
                Err(error) => Err(error),
            }
        }
        None => unreachable!("empty expression was checked above"),
    };
    match value {
        Ok(value) => expr_value_output(&value),
        Err(error) => expr_failure(error),
    }
}

fn expr_failure(message: impl Into<String>) -> CommandOutput {
    CommandOutput::failure(2, format!("expr: {}\n", message.into()))
}

fn expr_value_output(value: &ExprValue) -> CommandOutput {
    let is_false = match value {
        ExprValue::Integer(value) => *value == 0,
        ExprValue::Text(value) => value.is_empty(),
    };
    CommandOutput {
        stdout: format!("{}\n", value.display()),
        stderr: String::new(),
        status: i32::from(is_false),
    }
}

fn expr_length(arguments: &[String]) -> Result<ExprValue, String> {
    let [_, text] = arguments else {
        return Err("usage: expr length STRING".to_string());
    };
    let length = i64::try_from(text.chars().count())
        .map_err(|_| "string length exceeds integer range".to_string())?;
    Ok(ExprValue::Integer(length))
}

fn expr_index(arguments: &[String]) -> Result<ExprValue, String> {
    let [_, text, characters] = arguments else {
        return Err("usage: expr index STRING CHARACTERS".to_string());
    };
    let index = text
        .chars()
        .position(|character| characters.chars().any(|candidate| candidate == character))
        .map_or(0, |index| index + 1);
    let index = i64::try_from(index).map_err(|_| "index exceeds integer range".to_string())?;
    Ok(ExprValue::Integer(index))
}

fn expr_substr(arguments: &[String]) -> Result<ExprValue, String> {
    let [_, text, start, length] = arguments else {
        return Err("usage: expr substr STRING START LENGTH".to_string());
    };
    let start = parse_expr_integer(start, "START")?;
    let length = parse_expr_integer(length, "LENGTH")?;
    if start <= 0 || length <= 0 {
        return Ok(ExprValue::Text(String::new()));
    }
    let start = usize::try_from(start - 1).map_err(|_| "START is too large".to_string())?;
    let length = usize::try_from(length).map_err(|_| "LENGTH is too large".to_string())?;
    Ok(ExprValue::Text(
        text.chars().skip(start).take(length).collect(),
    ))
}

fn parse_expr_integer(value: &str, label: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|_| format!("{label} is not an integer: {value}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExprValue {
    Integer(i64),
    Text(String),
}

impl ExprValue {
    fn display(&self) -> String {
        match self {
            Self::Integer(value) => value.to_string(),
            Self::Text(value) => value.clone(),
        }
    }
}

struct ExprParser<'a> {
    arguments: &'a [String],
    position: usize,
}

impl<'a> ExprParser<'a> {
    fn new(arguments: &'a [String]) -> Self {
        Self {
            arguments,
            position: 0,
        }
    }

    fn is_at_end(&self) -> bool {
        self.position == self.arguments.len()
    }

    fn current(&self) -> Option<&str> {
        self.arguments.get(self.position).map(String::as_str)
    }

    fn advance(&mut self) -> Option<String> {
        let value = self.arguments.get(self.position).cloned();
        if value.is_some() {
            self.position += 1;
        }
        value
    }

    fn parse_comparison(&mut self) -> Result<ExprValue, String> {
        let mut left = self.parse_additive()?;
        while let Some(operator) = self
            .current()
            .filter(|operator| is_expr_comparison_operator(operator))
        {
            let operator = operator.to_string();
            self.advance();
            let right = self.parse_additive()?;
            left = ExprValue::Integer(i64::from(compare_expr_values(&left, &right, &operator)));
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<ExprValue, String> {
        let mut left = self.parse_multiplicative()?;
        while let Some(operator) = self
            .current()
            .filter(|operator| matches!(*operator, "+" | "-"))
        {
            let operator = operator.to_string();
            self.advance();
            let right = self.parse_multiplicative()?;
            left = apply_expr_arithmetic(left, right, &operator)?;
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<ExprValue, String> {
        let mut left = self.parse_primary()?;
        while let Some(operator) = self
            .current()
            .filter(|operator| matches!(*operator, "*" | "/" | "%"))
        {
            let operator = operator.to_string();
            self.advance();
            let right = self.parse_primary()?;
            left = apply_expr_arithmetic(left, right, &operator)?;
        }
        Ok(left)
    }

    fn parse_primary(&mut self) -> Result<ExprValue, String> {
        let value = self
            .advance()
            .ok_or_else(|| "missing operand".to_string())?;
        Ok(value
            .parse::<i64>()
            .map_or_else(|_| ExprValue::Text(value), ExprValue::Integer))
    }
}

fn is_expr_comparison_operator(operator: &str) -> bool {
    matches!(operator, "=" | "!=" | "<" | "<=" | ">" | ">=")
}

fn compare_expr_values(left: &ExprValue, right: &ExprValue, operator: &str) -> bool {
    let ordering = match (left, right) {
        (ExprValue::Integer(left), ExprValue::Integer(right)) => left.cmp(right),
        _ => left.display().cmp(&right.display()),
    };
    match operator {
        "=" => ordering.is_eq(),
        "!=" => !ordering.is_eq(),
        "<" => ordering.is_lt(),
        "<=" => !ordering.is_gt(),
        ">" => ordering.is_gt(),
        ">=" => !ordering.is_lt(),
        _ => false,
    }
}

fn apply_expr_arithmetic(
    left: ExprValue,
    right: ExprValue,
    operator: &str,
) -> Result<ExprValue, String> {
    let (ExprValue::Integer(left), ExprValue::Integer(right)) = (left, right) else {
        return Err(format!("operator {operator} requires integer operands"));
    };
    let value = match operator {
        "+" => left.checked_add(right),
        "-" => left.checked_sub(right),
        "*" => left.checked_mul(right),
        "/" => left.checked_div(right),
        "%" => left.checked_rem(right),
        _ => None,
    }
    .ok_or_else(|| {
        if matches!(operator, "/" | "%") && right == 0 {
            "division by zero".to_string()
        } else {
            format!("integer overflow for operator {operator}")
        }
    })?;
    Ok(ExprValue::Integer(value))
}

fn encode_base64(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4 + 1);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        output.push(char::from(BASE64_ALPHABET[usize::from(first >> 2)]));
        output.push(char::from(
            BASE64_ALPHABET[usize::from((first & 0x03) << 4 | second.unwrap_or(0) >> 4)],
        ));
        output.push(match second {
            Some(second) => char::from(
                BASE64_ALPHABET[usize::from((second & 0x0f) << 2 | third.unwrap_or(0) >> 6)],
            ),
            None => '=',
        });
        output.push(match third {
            Some(third) => char::from(BASE64_ALPHABET[usize::from(third & 0x3f)]),
            None => '=',
        });
    }
    output.push('\n');
    output
}

fn decode_base64(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    let compact = bytes
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if compact.len() % 4 != 0 {
        return Err("input length is not a multiple of four");
    }

    let mut output = Vec::with_capacity(compact.len() / 4 * 3);
    for (index, chunk) in compact.chunks(4).enumerate() {
        let last = (index + 1) * 4 == compact.len();
        let first = base64_value(chunk[0]).ok_or("invalid input character")?;
        let second = base64_value(chunk[1]).ok_or("invalid input character")?;
        if chunk[2] == b'=' {
            if chunk[3] != b'=' || !last || second & 0x0f != 0 {
                return Err("invalid padding");
            }
            output.push((first << 2) | (second >> 4));
            continue;
        }
        let third = base64_value(chunk[2]).ok_or("invalid input character")?;
        output.push((first << 2) | (second >> 4));
        output.push((second << 4) | (third >> 2));
        if chunk[3] == b'=' {
            if !last || third & 0x03 != 0 {
                return Err("invalid padding");
            }
        } else {
            let fourth = base64_value(chunk[3]).ok_or("invalid input character")?;
            output.push((third << 6) | fourth);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    BASE64_ALPHABET
        .iter()
        .position(|candidate| *candidate == byte)
        .and_then(|index| u8::try_from(index).ok())
}

fn posix_cksum(bytes: &[u8]) -> u32 {
    let mut table = [0_u32; 256];
    for (index, entry) in table.iter_mut().enumerate() {
        let mut value = u32::try_from(index).expect("CRC table index fits") << 24;
        for _ in 0..8 {
            value = if value & 0x8000_0000 != 0 {
                (value << 1) ^ 0x04c1_1db7
            } else {
                value << 1
            };
        }
        *entry = value;
    }

    let mut checksum = 0_u32;
    for byte in bytes {
        let index =
            usize::try_from((checksum >> 24) ^ u32::from(*byte)).expect("CRC table index fits");
        checksum = (checksum << 8) ^ table[index];
    }
    let mut length = bytes.len();
    while length > 0 {
        let byte = u8::try_from(length & 0xff).expect("CRC length byte fits");
        let index =
            usize::try_from((checksum >> 24) ^ u32::from(byte)).expect("CRC table index fits");
        checksum = (checksum << 8) ^ table[index];
        length >>= 8;
    }
    !checksum
}

fn md5_hex(bytes: &[u8]) -> String {
    let padded_len = (bytes.len() + 9).div_ceil(64) * 64;
    let mut padded = Vec::with_capacity(padded_len);
    padded.extend_from_slice(bytes);
    padded.push(0x80);
    padded.resize(padded_len - 8, 0);
    let bit_length = u64::try_from(bytes.len()).expect("MD5 input length fits") * 8;
    padded.extend_from_slice(&bit_length.to_le_bytes());

    let mut a = 0x6745_2301_u32;
    let mut b = 0xefcd_ab89_u32;
    let mut c = 0x98ba_dcfe_u32;
    let mut d = 0x1032_5476_u32;
    for block in padded.chunks_exact(64) {
        let mut words = [0_u32; 16];
        for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        let (original_a, original_b, original_c, original_d) = (a, b, c, d);
        for index in 0..64 {
            let (function, word_index) = match index {
                0..=15 => ((b & c) | (!b & d), index),
                16..=31 => ((d & b) | (!d & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let rotated = a
                .wrapping_add(function)
                .wrapping_add(MD5_CONSTANTS[index])
                .wrapping_add(words[word_index])
                .rotate_left(MD5_SHIFTS[index]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        a = a.wrapping_add(original_a);
        b = b.wrapping_add(original_b);
        c = c.wrapping_add(original_c);
        d = d.wrapping_add(original_d);
    }

    let mut output = String::with_capacity(32);
    for word in [a, b, c, d] {
        for byte in word.to_le_bytes() {
            let _ = write!(output, "{byte:02x}");
        }
    }
    output
}

pub(super) fn dirname(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 1 || context.args[0].is_empty() {
        return usage("dirname", "usage: dirname PATH");
    }
    CommandOutput::success(format!("{}\n", dirname_value(&context.args[0])))
}

pub(super) fn realpath(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut paths = Vec::new();
    let mut parse_options = true;
    for argument in context.args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument.starts_with('-') {
            return usage("realpath", "usage: realpath [--] PATH ...");
        } else {
            paths.push(argument.as_str());
        }
    }
    if paths.is_empty() {
        return usage("realpath", "usage: realpath [--] PATH ...");
    }

    let mut output = CommandOutput::success("");
    for path in paths {
        match context.fs.canonical_path(path) {
            Ok(canonical) => {
                output.stdout.push_str(&canonical);
                output.stdout.push('\n');
            }
            Err(error) => {
                output.status = 1;
                let _ = writeln!(output.stderr, "realpath: {path}: {error}");
            }
        }
    }
    output
}

pub(super) fn sha256(context: &mut CommandContext<'_>) -> CommandOutput {
    let path = match context.args {
        [] => "-",
        [path] => path.as_str(),
        [flag, path] if flag == "--" => path.as_str(),
        _ => return usage("sha256", "usage: sha256 [--] [FILE]"),
    };
    let bytes = if path == "-" {
        context.stdin.as_bytes().to_vec()
    } else {
        match context.fs.read(path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure("sha256", &error),
        }
    };
    CommandOutput::success(format!("{}  {path}\n", sha256_hex(&bytes)))
}

pub(super) fn du(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() > 1 {
        return usage("du", "usage: du [PATH]");
    }
    let path = context.args.first().map_or("~", String::as_str);
    let mut visited = 0;
    match disk_usage(context, path, &mut visited) {
        Ok(bytes) => CommandOutput::success(format!("{bytes}\t{path}\n")),
        Err(output) => output,
    }
}

pub(super) fn rmdir(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("rmdir", "usage: rmdir DIRECTORY ...");
    }
    for path in context.args {
        let info = match context.fs.metadata(path) {
            Ok(info) => info,
            Err(error) => return fs_failure("rmdir", &error),
        };
        if !info.is_directory {
            return fs_failure("rmdir", &rune_fs::FsError::NotDirectory(path.clone()));
        }
        if let Err(error) = context.fs.remove(path, false, false) {
            return fs_failure("rmdir", &error);
        }
    }
    CommandOutput::success("")
}

pub(super) fn stat(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 1 {
        return usage("stat", "usage: stat PATH");
    }
    let path = &context.args[0];
    let info = match context.fs.metadata(path) {
        Ok(info) => info,
        Err(error) => return fs_failure("stat", &error),
    };
    let kind = if info.is_symlink {
        "symlink"
    } else if info.is_directory {
        "directory"
    } else {
        "file"
    };
    CommandOutput::success(format!(
        "  File: {path}\n  Name: {}\n  Size: {}\n  Type: {kind}\n",
        info.name, info.size
    ))
}

pub(super) fn file(context: &mut CommandContext<'_>) -> CommandOutput {
    let (brief, mime_type, paths) = match parse_file_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let mut output = CommandOutput::success("");
    for path in paths {
        let description = match describe_file(context, path, mime_type) {
            Ok(description) => description,
            Err(error) => {
                output.status = 1;
                let _ = writeln!(output.stderr, "file: {path}: {error}");
                continue;
            }
        };
        if !brief {
            output.stdout.push_str(path);
            output.stdout.push_str(": ");
        }
        output.stdout.push_str(&description);
        output.stdout.push('\n');
    }
    output
}

fn parse_file_args(args: &[String]) -> Result<(bool, bool, Vec<&str>), CommandOutput> {
    let mut brief = false;
    let mut mime_type = false;
    let mut paths = Vec::new();
    let mut parse_options = true;
    for argument in args {
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && matches!(argument.as_str(), "-b" | "--brief") {
            brief = true;
        } else if parse_options && argument == "--mime-type" {
            mime_type = true;
        } else if parse_options && argument.starts_with('-') && argument != "-" {
            return Err(usage(
                "file",
                "usage: file [-b|--brief] [--mime-type] [--] FILE ...",
            ));
        } else {
            paths.push(argument.as_str());
        }
    }
    if paths.is_empty() {
        return Err(usage(
            "file",
            "usage: file [-b|--brief] [--mime-type] [--] FILE ...",
        ));
    }
    Ok((brief, mime_type, paths))
}

fn describe_file(
    context: &mut CommandContext<'_>,
    path: &str,
    mime_type: bool,
) -> Result<String, rune_fs::FsError> {
    let bytes = if path == "-" {
        context.stdin.as_bytes().to_vec()
    } else {
        let info = context.fs.metadata(path)?;
        if info.is_symlink {
            return Ok(if mime_type {
                "inode/symlink".to_string()
            } else {
                "symbolic link".to_string()
            });
        }
        if info.is_directory {
            return Ok(if mime_type {
                "inode/directory".to_string()
            } else {
                "directory".to_string()
            });
        }
        if info.size > MAX_FILE_PROBE_BYTES as u64 {
            return Err(rune_fs::FsError::Io {
                operation: "probe".to_string(),
                path: path.to_string(),
                message: format!("file exceeds the {MAX_FILE_PROBE_BYTES}-byte probe limit"),
            });
        }
        context.fs.read(path)?
    };
    if bytes.len() > MAX_FILE_PROBE_BYTES {
        return Err(rune_fs::FsError::Io {
            operation: "probe".to_string(),
            path: path.to_string(),
            message: format!("file exceeds the {MAX_FILE_PROBE_BYTES}-byte probe limit"),
        });
    }
    Ok(classify_file_bytes(&bytes, mime_type))
}

fn classify_file_bytes(bytes: &[u8], mime_type: bool) -> String {
    if bytes.is_empty() {
        return if mime_type {
            "application/x-empty".to_string()
        } else {
            "empty".to_string()
        };
    }
    let (description, mime) = if bytes.starts_with(b"\x7fELF") {
        ("ELF binary", "application/x-elf")
    } else if bytes.starts_with(b"\0asm") {
        ("WebAssembly binary", "application/wasm")
    } else if bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
    {
        ("Zip archive", "application/zip")
    } else if bytes.starts_with(b"\x1f\x8b") {
        ("gzip compressed data", "application/gzip")
    } else if bytes.len() >= 262 && &bytes[257..262] == b"ustar" {
        ("POSIX tar archive", "application/x-tar")
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        if text.chars().all(is_file_text_character) {
            if bytes.iter().any(|byte| *byte >= 0x80) {
                ("UTF-8 Unicode text", "text/plain; charset=utf-8")
            } else {
                ("ASCII text", "text/plain; charset=us-ascii")
            }
        } else {
            ("data", "application/octet-stream")
        }
    } else {
        ("data", "application/octet-stream")
    };
    if mime_type {
        mime.to_string()
    } else {
        description.to_string()
    }
}

fn is_file_text_character(character: char) -> bool {
    character == '\n'
        || character == '\r'
        || character == '\t'
        || character == '\u{0c}'
        || !character.is_control()
}

pub(super) fn tee(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut append = false;
    let mut paths = Vec::new();
    for argument in context.args {
        match argument.as_str() {
            "-a" => append = true,
            "--" => {}
            _ if argument.starts_with('-') => {
                return usage("tee", "usage: tee [-a] [FILE ...]");
            }
            _ => paths.push(argument),
        }
    }
    for path in paths {
        if let Err(error) = context.fs.write(path, context.stdin.as_bytes(), append) {
            return fs_failure("tee", &error);
        }
    }
    CommandOutput::success(context.stdin)
}

pub(super) fn seq(context: &mut CommandContext<'_>) -> CommandOutput {
    let (first_text, increment_text, last_text) = match context.args {
        [last] => ("0", "1", last.as_str()),
        [first, last] => (first.as_str(), "1", last.as_str()),
        [first, increment, last] => (first.as_str(), increment.as_str(), last.as_str()),
        _ => return usage("seq", "usage: seq [FIRST [INCREMENT]] LAST"),
    };
    let parse = |text: &str| text.parse::<i64>().map(i128::from);
    let Ok(first) = parse(first_text) else {
        return CommandOutput::failure(1, format!("seq: invalid integer: {first_text}\n"));
    };
    let Ok(increment) = parse(increment_text) else {
        return CommandOutput::failure(1, format!("seq: invalid integer: {increment_text}\n"));
    };
    let Ok(last) = parse(last_text) else {
        return CommandOutput::failure(1, format!("seq: invalid integer: {last_text}\n"));
    };
    if increment == 0 {
        return CommandOutput::failure(1, "seq: increment must not be zero\n");
    }
    let count = if (increment > 0 && first > last) || (increment < 0 && first < last) {
        0
    } else if increment > 0 {
        ((last - first) / increment) + 1
    } else {
        ((first - last) / -increment) + 1
    };
    if count > MAX_SEQ_VALUES as i128 {
        return CommandOutput::failure(1, format!("seq: output exceeds {MAX_SEQ_VALUES} values\n"));
    }
    let mut output = String::new();
    let mut current = first;
    for _ in 0..count {
        let _ = writeln!(output, "{current}");
        current += increment;
    }
    CommandOutput::success(output)
}

pub(super) fn tr(context: &mut CommandContext<'_>) -> CommandOutput {
    let (delete, set_one, set_two) = match context.args {
        [flag, set_one] if flag == "-d" => (true, set_one.as_str(), None),
        [set_one, set_two] => (false, set_one.as_str(), Some(set_two.as_str())),
        _ => return usage("tr", "usage: tr [-d] SET1 [SET2]"),
    };
    if set_one.is_empty() || (!delete && set_two.is_some_and(str::is_empty)) {
        return usage("tr", "SET1 and SET2 must not be empty");
    }
    let source = set_one.chars().collect::<Vec<_>>();
    let target = set_two.map(|value| value.chars().collect::<Vec<_>>());
    let mut stdout = String::with_capacity(context.stdin.len());
    for character in context.stdin.chars() {
        let Some(index) = source.iter().position(|candidate| *candidate == character) else {
            stdout.push(character);
            continue;
        };
        if delete {
            continue;
        }
        let Some(target) = target.as_ref() else {
            return usage("tr", "SET2 is required unless -d is used");
        };
        stdout.push(target[index.min(target.len().saturating_sub(1))]);
    }
    CommandOutput::success(stdout)
}

pub(super) fn unlink(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("unlink", "usage: unlink FILE");
    }
    if context.args.len() != 1 {
        return usage("unlink", "usage: unlink FILE");
    }
    let path = &context.args[0];
    match context.fs.metadata(path) {
        Ok(info) if info.is_directory => {
            fs_failure("unlink", &rune_fs::FsError::NotFile(path.clone()))
        }
        Ok(_) => context.fs.remove(path, false, false).map_or_else(
            |error| fs_failure("unlink", &error),
            |()| CommandOutput::success(""),
        ),
        Err(error) => fs_failure("unlink", &error),
    }
}

pub(super) fn xxd(context: &mut CommandContext<'_>) -> CommandOutput {
    let (plain, path) = match context.args {
        [] => (false, None),
        [flag] if flag == "-p" => (true, None),
        [path] => (false, Some(path.as_str())),
        [flag, path] if flag == "-p" => (true, Some(path.as_str())),
        _ => return usage("xxd", "usage: xxd [-p] [FILE]"),
    };
    let bytes = match path {
        Some(path) => match context.fs.read(path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure("xxd", &error),
        },
        None => context.stdin.as_bytes().to_vec(),
    };
    if bytes.len() > MAX_HEXDUMP_INPUT {
        return CommandOutput::failure(
            1,
            format!("xxd: input exceeds {MAX_HEXDUMP_INPUT} bytes\n"),
        );
    }
    if plain {
        return CommandOutput::success(plain_hex(&bytes));
    }
    CommandOutput::success(classic_hex(&bytes))
}

fn basename_value(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed.rsplit('/').next().unwrap_or(trimmed)
    }
}

fn dirname_value(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/";
    }
    match trimmed.rfind('/') {
        None => ".",
        Some(0) => "/",
        Some(index) => &trimmed[..index],
    }
}

fn disk_usage(
    context: &mut CommandContext<'_>,
    path: &str,
    visited: &mut usize,
) -> Result<u64, CommandOutput> {
    if let Some(output) = context.take_cancellation() {
        return Err(output);
    }
    if *visited >= MAX_DISK_USAGE_ENTRIES {
        return Err(CommandOutput::failure(
            1,
            format!("du: traversal exceeded {MAX_DISK_USAGE_ENTRIES} entries\n"),
        ));
    }
    let info = context
        .fs
        .metadata(path)
        .map_err(|error| fs_failure("du", &error))?;
    *visited += 1;
    if !info.is_directory || info.is_symlink {
        return Ok(info.size);
    }
    let entries = context
        .fs
        .list(Some(path))
        .map_err(|error| fs_failure("du", &error))?;
    let mut total = 0_u64;
    for entry in entries {
        let child = if path == "/" {
            format!("/{name}", name = entry.name)
        } else if path.ends_with('/') {
            format!("{path}{name}", name = entry.name)
        } else {
            format!("{path}/{name}", name = entry.name)
        };
        total = total.saturating_add(disk_usage(context, &child, visited)?);
    }
    Ok(total)
}

fn plain_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2 + 1);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output.push('\n');
    output
}

fn classic_hex(bytes: &[u8]) -> String {
    let mut output = String::new();
    for (line, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(output, "{line:08x}: ");
        for index in 0..16 {
            if let Some(byte) = chunk.get(index) {
                let _ = write!(output, "{byte:02x} ");
            } else {
                output.push_str("   ");
            }
            if index == 7 {
                output.push(' ');
            }
        }
        output.push(' ');
        for byte in chunk {
            output.push(if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            });
        }
        output.push('\n');
    }
    output
}
