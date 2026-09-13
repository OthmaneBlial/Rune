use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::io::{Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

const MAX_GZIP_BYTES: usize = 64 * 1024 * 1024;
const LZW_MAGIC: [u8; 2] = [0x1f, 0x9d];
const LZW_MAX_BITS: u8 = 16;
const LZW_ENCODER_MAX_BITS: u8 = 9;
const LZW_BLOCK_MODE: u8 = 0x80;
const LZW_CLEAR_CODE: usize = 256;
const LZW_FIRST_FREE_CODE: usize = 257;

/// Compresses VFS files to sibling `.gz` files without deleting the source.
pub(super) fn gzip(context: &mut CommandContext<'_>) -> CommandOutput {
    run(context, false, "gzip")
}

/// Decompresses VFS `.gz` files to sibling files without deleting the source.
pub(super) fn gunzip(context: &mut CommandContext<'_>) -> CommandOutput {
    run(context, true, "gunzip")
}

/// Compresses VFS files to sibling `.Z` files using bounded LZW.
pub(super) fn compress(context: &mut CommandContext<'_>) -> CommandOutput {
    run_lzw(context, false)
}

/// Decompresses VFS `.Z` files to sibling files using bounded LZW.
pub(super) fn uncompress(context: &mut CommandContext<'_>) -> CommandOutput {
    run_lzw(context, true)
}

fn run(context: &mut CommandContext<'_>, mut decompress: bool, command: &str) -> CommandOutput {
    let mut paths = Vec::new();
    let mut options_done = false;
    for argument in context.args {
        if !options_done && argument == "--" {
            options_done = true;
            continue;
        }
        if !options_done && command == "gzip" && matches!(argument.as_str(), "-d" | "--decompress")
        {
            decompress = true;
            continue;
        }
        if !options_done && argument.starts_with('-') {
            return usage(
                command,
                if decompress {
                    "usage: gunzip [--] FILE ..."
                } else {
                    "usage: gzip [-d|--decompress] [--] FILE ..."
                },
            );
        }
        paths.push(argument.as_str());
    }
    if paths.is_empty() {
        return usage(
            command,
            if decompress {
                "usage: gunzip [--] FILE ..."
            } else {
                "usage: gzip [-d|--decompress] [--] FILE ..."
            },
        );
    }

    let mut output = String::new();
    for path in paths {
        if let Some(cancelled) = context.take_cancellation() {
            return cancelled;
        }
        let destination = match destination_path(path, decompress) {
            Ok(destination) => destination,
            Err(error) => return compression_failure(command, path, error),
        };
        if context.fs.metadata(&destination).is_ok() {
            return compression_failure(command, &destination, "destination already exists");
        }
        let input = match context.fs.read(path) {
            Ok(input) => input,
            Err(error) => return fs_failure(command, &error),
        };
        if input.len() > MAX_GZIP_BYTES {
            return compression_failure(command, path, "input exceeds the 64 MiB limit");
        }
        let bytes = if decompress {
            match decompress_gzip(&input) {
                Ok(bytes) => bytes,
                Err(error) => return compression_failure(command, path, &error),
            }
        } else {
            match compress_gzip(&input) {
                Ok(bytes) => bytes,
                Err(error) => return compression_failure(command, path, &error),
            }
        };
        if let Err(error) = context.fs.write(&destination, &bytes, false) {
            return fs_failure(command, &error);
        }
        let verb = if decompress {
            "decompressed"
        } else {
            "compressed"
        };
        let _ = writeln!(output, "{verb} {path} -> {destination}");
    }
    CommandOutput::success(output)
}

fn destination_path(path: &str, decompress: bool) -> Result<String, &'static str> {
    if decompress {
        let Some(destination) = path.strip_suffix(".gz") else {
            return Err("input must end in .gz");
        };
        if destination.is_empty() {
            return Err("input must include a filename before .gz");
        }
        Ok(destination.to_string())
    } else {
        Ok(format!("{path}.gz"))
    }
}

fn compress_gzip(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(input)
        .map_err(|error| format!("compression failed: {error}"))?;
    let output = encoder
        .finish()
        .map_err(|error| format!("compression failed: {error}"))?;
    if output.len() > MAX_GZIP_BYTES {
        return Err("compressed output exceeds the 64 MiB limit".to_string());
    }
    Ok(output)
}

fn decompress_gzip(input: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = GzDecoder::new(input);
    let mut bounded = decoder.take((MAX_GZIP_BYTES + 1) as u64);
    let mut output = Vec::new();
    bounded
        .read_to_end(&mut output)
        .map_err(|error| format!("decompression failed: {error}"))?;
    if output.len() > MAX_GZIP_BYTES {
        return Err("decompressed output exceeds the 64 MiB limit".to_string());
    }
    Ok(output)
}

fn compression_failure(command: &str, path: &str, message: &str) -> CommandOutput {
    CommandOutput::failure(1, format!("{command}: {path}: {message}\n"))
}

fn run_lzw(context: &mut CommandContext<'_>, decompress: bool) -> CommandOutput {
    let command = if decompress { "uncompress" } else { "compress" };
    let mut paths = Vec::new();
    let mut options_done = false;
    for argument in context.args {
        if !options_done && argument == "--" {
            options_done = true;
            continue;
        }
        if !options_done && argument.starts_with('-') {
            return usage(
                command,
                if decompress {
                    "usage: uncompress [--] FILE.Z ..."
                } else {
                    "usage: compress [--] FILE ..."
                },
            );
        }
        paths.push(argument.as_str());
    }
    if paths.is_empty() {
        return usage(
            command,
            if decompress {
                "usage: uncompress [--] FILE.Z ..."
            } else {
                "usage: compress [--] FILE ..."
            },
        );
    }

    let mut output = String::new();
    for path in paths {
        if let Some(cancelled) = context.take_cancellation() {
            return cancelled;
        }
        let destination = match lzw_destination_path(path, decompress) {
            Ok(destination) => destination,
            Err(error) => return compression_failure(command, path, error),
        };
        if context.fs.metadata(&destination).is_ok() {
            return compression_failure(command, &destination, "destination already exists");
        }
        let input = match context.fs.read(path) {
            Ok(input) => input,
            Err(error) => return fs_failure(command, &error),
        };
        if input.len() > MAX_GZIP_BYTES {
            return compression_failure(command, path, "input exceeds the 64 MiB limit");
        }
        let bytes = if decompress {
            match decompress_lzw(&input) {
                Ok(bytes) => bytes,
                Err(error) => return compression_failure(command, path, &error),
            }
        } else {
            match compress_lzw(&input) {
                Ok(bytes) => bytes,
                Err(error) => return compression_failure(command, path, &error),
            }
        };
        if let Err(error) = context.fs.write(&destination, &bytes, false) {
            return fs_failure(command, &error);
        }
        let verb = if decompress {
            "decompressed"
        } else {
            "compressed"
        };
        let _ = writeln!(output, "{verb} {path} -> {destination}");
    }
    CommandOutput::success(output)
}

fn lzw_destination_path(path: &str, decompress: bool) -> Result<String, &'static str> {
    if decompress {
        let Some(destination) = path.strip_suffix(".Z") else {
            return Err("input must end in .Z");
        };
        if destination.is_empty() {
            return Err("input must include a filename before .Z");
        }
        Ok(destination.to_string())
    } else {
        Ok(format!("{path}.Z"))
    }
}

fn compress_lzw(input: &[u8]) -> Result<Vec<u8>, String> {
    let output = vec![
        LZW_MAGIC[0],
        LZW_MAGIC[1],
        LZW_BLOCK_MODE | LZW_ENCODER_MAX_BITS,
    ];
    let mut writer = BitWriter::new(output);
    let mut dictionary = HashMap::with_capacity(1 << LZW_ENCODER_MAX_BITS);
    let mut next_code = LZW_FIRST_FREE_CODE;
    let mut code_width = 9;
    let mut bytes = input.iter().copied();
    let Some(first) = bytes.next() else {
        return writer.finish(MAX_GZIP_BYTES);
    };
    let mut current = usize::from(first);
    for byte in bytes {
        if let Some(&code) = dictionary.get(&(current, byte)) {
            current = code;
            continue;
        }
        writer.write(current, code_width, MAX_GZIP_BYTES)?;
        if next_code < (1_usize << LZW_ENCODER_MAX_BITS) {
            dictionary.insert((current, byte), next_code);
            next_code += 1;
            if next_code == (1_usize << code_width) && code_width < LZW_ENCODER_MAX_BITS {
                code_width += 1;
            }
        } else {
            writer.write(LZW_CLEAR_CODE, code_width, MAX_GZIP_BYTES)?;
            dictionary.clear();
            next_code = LZW_FIRST_FREE_CODE;
            code_width = 9;
        }
        current = usize::from(byte);
    }
    writer.write(current, code_width, MAX_GZIP_BYTES)?;
    writer.finish(MAX_GZIP_BYTES)
}

fn decompress_lzw(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() < 3 || input[..2] != LZW_MAGIC {
        return Err("input is not a .Z stream".to_string());
    }
    let flags = input[2];
    let max_bits = flags & 0x1f;
    if !(9..=LZW_MAX_BITS).contains(&max_bits) || flags & 0x60 != 0 {
        return Err(".Z stream uses unsupported flags".to_string());
    }
    if flags & LZW_BLOCK_MODE == 0 {
        return Err(".Z stream without block mode is unsupported".to_string());
    }
    let max_codes = 1_usize << max_bits;
    let mut reader = BitReader::new(&input[3..]);
    let mut prefix = vec![0_u16; max_codes];
    let mut suffix = vec![0_u8; max_codes];
    let mut next_code = LZW_FIRST_FREE_CODE;
    let mut code_width = 9;
    let mut old_code = None;
    let mut output = Vec::new();

    while let Some(code) = reader.read(code_width) {
        if code == LZW_CLEAR_CODE {
            next_code = LZW_FIRST_FREE_CODE;
            code_width = 9;
            old_code = None;
            continue;
        }
        if code > 255 && old_code.is_none() {
            return Err(".Z stream starts with an invalid code".to_string());
        }
        let sequence = if code == next_code {
            let Some(old_code) = old_code else {
                return Err(".Z stream has an invalid dictionary reference".to_string());
            };
            let mut sequence = expand_lzw_code(old_code, &prefix, &suffix, next_code, max_codes)?;
            let Some(&first) = sequence.first() else {
                return Err(".Z stream has an empty dictionary sequence".to_string());
            };
            sequence.push(first);
            sequence
        } else {
            if code >= next_code && code > 255 {
                return Err(".Z stream references an unknown code".to_string());
            }
            expand_lzw_code(code, &prefix, &suffix, next_code, max_codes)?
        };
        if output.len().saturating_add(sequence.len()) > MAX_GZIP_BYTES {
            return Err("decompressed output exceeds the 64 MiB limit".to_string());
        }
        let first = sequence[0];
        output.extend_from_slice(&sequence);
        if let Some(old_code) = old_code {
            if next_code < max_codes {
                prefix[next_code] = u16::try_from(old_code)
                    .map_err(|_| ".Z stream has an invalid dictionary prefix".to_string())?;
                suffix[next_code] = first;
                next_code += 1;
                if next_code == (1_usize << code_width) && code_width < max_bits {
                    code_width += 1;
                }
            }
        }
        old_code = Some(code);
    }
    Ok(output)
}

fn expand_lzw_code(
    mut code: usize,
    prefix: &[u16],
    suffix: &[u8],
    next_code: usize,
    max_codes: usize,
) -> Result<Vec<u8>, String> {
    if code >= max_codes || (code >= 256 && code >= next_code) {
        return Err(".Z stream references an unknown code".to_string());
    }
    let mut sequence = Vec::new();
    let mut steps = 0;
    while code > 255 {
        sequence.push(suffix[code]);
        code = usize::from(prefix[code]);
        steps += 1;
        if steps >= max_codes {
            return Err(".Z stream contains a dictionary cycle".to_string());
        }
    }
    sequence
        .push(u8::try_from(code).map_err(|_| ".Z stream has an invalid literal code".to_string())?);
    sequence.reverse();
    Ok(sequence)
}

struct BitWriter {
    bytes: Vec<u8>,
    buffer: u32,
    bits: u8,
}

impl BitWriter {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            buffer: 0,
            bits: 0,
        }
    }

    fn write(&mut self, code: usize, width: u8, maximum: usize) -> Result<(), String> {
        let code = u32::try_from(code).map_err(|_| "LZW code exceeds 32 bits".to_string())?;
        self.buffer |= code << self.bits;
        self.bits += width;
        while self.bits >= 8 {
            self.bytes.push(
                u8::try_from(self.buffer & 0xff)
                    .map_err(|_| "LZW byte buffer is invalid".to_string())?,
            );
            self.buffer >>= 8;
            self.bits -= 8;
            if self.bytes.len() > maximum {
                return Err("compressed output exceeds the 64 MiB limit".to_string());
            }
        }
        Ok(())
    }

    fn finish(mut self, maximum: usize) -> Result<Vec<u8>, String> {
        if self.bits > 0 {
            self.bytes.push(
                u8::try_from(self.buffer & 0xff)
                    .map_err(|_| "LZW byte buffer is invalid".to_string())?,
            );
        }
        if self.bytes.len() > maximum {
            return Err("compressed output exceeds the 64 MiB limit".to_string());
        }
        Ok(self.bytes)
    }
}

struct BitReader<'a> {
    bytes: &'a [u8],
    index: usize,
    buffer: u32,
    bits: u8,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            index: 0,
            buffer: 0,
            bits: 0,
        }
    }

    fn read(&mut self, width: u8) -> Option<usize> {
        while self.bits < width {
            let byte = *self.bytes.get(self.index)?;
            self.index += 1;
            self.buffer |= u32::from(byte) << self.bits;
            self.bits += 8;
        }
        let mask = (1_u32 << width) - 1;
        let code = self.buffer & mask;
        self.buffer >>= width;
        self.bits -= width;
        Some(code as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::{compress_lzw, decompress_lzw};

    #[test]
    fn lzw_round_trips_dictionary_growth() {
        for length in [256, 384, 448, 480, 512, 1_000, 2_000, 5_000, 20_000] {
            let input: Vec<u8> = (0_usize..length)
                .map(|index| u8::try_from(index % 251).expect("pattern fits in a byte"))
                .collect();
            let compressed = compress_lzw(&input).expect("LZW compression succeeds");
            let decompressed = decompress_lzw(&compressed).unwrap_or_else(|error| {
                panic!("LZW decompression fails at {length} bytes: {error}")
            });
            assert_eq!(decompressed, input, "round-trip failed at {length} bytes");
        }
    }
}
