use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};
use rune_package::sha256_hex;

const MAX_HEXDUMP_INPUT: usize = 256 * 1024;
const MAX_BASE64_INPUT: usize = 768 * 1024;
const MAX_CKSUM_INPUT: usize = 16 * 1024 * 1024;
const MAX_MD5_INPUT: usize = 16 * 1024 * 1024;
const MAX_DISK_USAGE_ENTRIES: usize = 10_000;

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
        let target = target.as_ref().expect("translation has a target");
        stdout.push(target[index.min(target.len() - 1)]);
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
