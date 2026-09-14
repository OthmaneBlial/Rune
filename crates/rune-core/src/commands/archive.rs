use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};

use crate::{fs_failure, usage, CommandContext, CommandOutput};
use flate2::read::{DeflateDecoder, GzDecoder};
use flate2::write::DeflateEncoder;
use flate2::write::GzEncoder;
use flate2::Compression;

const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_ARCHIVE_PATH_BYTES: usize = 1_024;
const ZIP_DATA_DESCRIPTOR_FLAG: u16 = 0x0008;
const ZIP_UTF8_FLAG: u16 = 0x0800;
const ZIP_ALLOWED_FLAGS: u16 = ZIP_DATA_DESCRIPTOR_FLAG | ZIP_UTF8_FLAG;
const ZIP_DATA_DESCRIPTOR_SIGNATURE: u32 = 0x0807_4b50;

#[derive(Debug)]
struct ArchiveEntry {
    name: String,
    bytes: Vec<u8>,
    directory: bool,
}

#[derive(Debug, Clone, Copy)]
struct ZipEntryMetadata {
    flags: u16,
    method: u16,
    compressed_size: usize,
    uncompressed_size: usize,
    crc: u32,
}

pub(super) fn zip(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut recursive = false;
    let mut positional = Vec::new();
    for argument in context.args {
        match argument.as_str() {
            "-r" | "--recurse-paths" => recursive = true,
            "--" => {}
            _ if argument.starts_with('-') => {
                return usage("zip", "usage: zip [-r] ARCHIVE FILE ...");
            }
            _ => positional.push(argument.as_str()),
        }
    }
    if positional.len() < 2 {
        return usage("zip", "usage: zip [-r] ARCHIVE FILE ...");
    }
    let archive_path = positional[0];
    let mut entries = Vec::new();
    for path in &positional[1..] {
        let name = archive_name(path);
        let name = if name == "." { String::new() } else { name };
        if let Err(output) = collect_entries(context, path, &name, recursive, &mut entries, "zip") {
            return output;
        }
    }
    let archive = match build_archive(&entries) {
        Ok(archive) => archive,
        Err(error) => return archive_failure(&error),
    };
    if let Err(error) = context.fs.write(archive_path, &archive, false) {
        return fs_failure("zip", &error);
    }
    CommandOutput::success(format!(
        "created {archive_path} ({} entries)\n",
        entries.len()
    ))
}

pub(super) fn unzip(context: &mut CommandContext<'_>) -> CommandOutput {
    let parsed = match parse_unzip_arguments(context.args) {
        Ok(parsed) => parsed,
        Err(error) => return usage("unzip", &error),
    };
    let archive_path = parsed.archive;
    let archive = match context.fs.read(archive_path) {
        Ok(bytes) => bytes,
        Err(error) => return fs_failure("unzip", &error),
    };
    if archive.len() > MAX_ARCHIVE_BYTES {
        return archive_failure("archive exceeds the 64 MiB limit");
    }
    let entries = match read_central_directory(&archive) {
        Ok(entries) => entries,
        Err(error) => return archive_failure(&error),
    };
    let selected = match select_zip_entries(&entries, &parsed.filters) {
        Ok(selected) => selected,
        Err(error) => return archive_failure(&error),
    };
    let destination = parsed.destination.as_str();
    if let Err(error) = context.fs.make_directory(destination, true) {
        return fs_failure("unzip", &error);
    }
    for index in &selected {
        let entry = &entries[*index];
        if let Some(output) = context.take_cancellation() {
            return output;
        }
        let output_path = append_path(destination, &entry.name);
        if entry.directory {
            if let Err(error) = context.fs.make_directory(&output_path, true) {
                return fs_failure("unzip", &error);
            }
            continue;
        }
        let parent =
            output_path.rsplit_once('/').map_or(
                ".",
                |(parent, _)| if parent.is_empty() { "/" } else { parent },
            );
        if let Err(error) = context.fs.make_directory(parent, true) {
            return fs_failure("unzip", &error);
        }
        if let Err(error) = context.fs.write(&output_path, &entry.bytes, false) {
            return fs_failure("unzip", &error);
        }
    }
    CommandOutput::success(format!(
        "extracted {} entries into {destination}\n",
        selected.len()
    ))
}

#[derive(Debug)]
struct UnzipArguments<'a> {
    archive: &'a str,
    destination: String,
    filters: Vec<&'a str>,
}

fn parse_unzip_arguments(arguments: &[String]) -> Result<UnzipArguments<'_>, String> {
    let archive = arguments
        .first()
        .map(String::as_str)
        .ok_or("usage: unzip ARCHIVE [DESTINATION]")?;
    let mut positionals = Vec::new();
    let mut destination = None;
    let mut parse_options = true;
    let mut index = 1;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && matches!(argument, "-d" | "--directory") {
            index += 1;
            let value = arguments
                .get(index)
                .ok_or("-d requires a destination")?
                .as_str();
            if value.is_empty() {
                return Err("-d requires a non-empty destination".to_string());
            }
            if destination.replace(value.to_string()).is_some() {
                return Err("unzip accepts exactly one destination".to_string());
            }
        } else if parse_options && argument.starts_with('-') {
            return Err(format!("unsupported unzip option: {argument}"));
        } else {
            positionals.push(argument);
        }
        index += 1;
    }
    let (destination, filters) = match destination {
        Some(destination) => (destination, positionals),
        None => match positionals.as_slice() {
            [] => (".".to_string(), Vec::new()),
            [destination] => ((*destination).to_string(), Vec::new()),
            _ => return Err(
                "usage: unzip ARCHIVE [DESTINATION] or unzip ARCHIVE -d DESTINATION [MEMBER ...]"
                    .to_string(),
            ),
        },
    };
    Ok(UnzipArguments {
        archive,
        destination,
        filters,
    })
}

/// Creates, lists, or extracts a bounded USTAR archive, optionally gzip-compressed.
///
/// The implementation deliberately keeps the supported surface explicit:
/// regular files and directories only, optional gzip compression, no links,
/// device nodes, PAX extensions, or host-process fallback. `-C` applies to
/// extraction.
pub(super) fn tar(context: &mut CommandContext<'_>) -> CommandOutput {
    let parsed = match parse_tar_arguments(context.args) {
        Ok(parsed) => parsed,
        Err(error) => return archive_failure_for("tar", &error),
    };
    match parsed.mode {
        TarMode::Create => tar_create(context, &parsed),
        TarMode::List => tar_list(context, &parsed),
        TarMode::Extract => tar_extract(context, &parsed),
    }
}

fn tar_create(context: &mut CommandContext<'_>, parsed: &TarArguments<'_>) -> CommandOutput {
    if parsed.destination.is_some() {
        return archive_failure_for("tar", "-C is only supported with extraction");
    }
    if parsed.paths.is_empty() {
        return usage("tar", "usage: tar [-z|--gzip] -cf ARCHIVE FILE ...");
    }
    let mut entries = Vec::new();
    for path in &parsed.paths {
        let name = archive_name(path);
        let name = if name == "." { String::new() } else { name };
        if let Err(output) = collect_entries(context, path, &name, true, &mut entries, "tar") {
            return output;
        }
    }
    let archive = match build_tar_archive(&entries) {
        Ok(archive) => archive,
        Err(error) => return archive_failure_for("tar", &error),
    };
    let archive = if parsed.gzip {
        match compress_tar_archive(&archive) {
            Ok(archive) => archive,
            Err(error) => return archive_failure_for("tar", &error),
        }
    } else {
        archive
    };
    if let Err(error) = context.fs.write(parsed.archive, &archive, false) {
        return fs_failure("tar", &error);
    }
    if parsed.verbose {
        return entry_names(&entries);
    }
    CommandOutput::success(format!(
        "created {} ({} entries)\n",
        parsed.archive,
        entries.len()
    ))
}

fn tar_list(context: &mut CommandContext<'_>, parsed: &TarArguments<'_>) -> CommandOutput {
    if parsed.destination.is_some() {
        return archive_failure_for("tar", "-C is only supported with extraction");
    }
    let archive = match context.fs.read(parsed.archive) {
        Ok(bytes) => bytes,
        Err(error) => return fs_failure("tar", &error),
    };
    let entries = match read_tar_entries(&archive, parsed.gzip) {
        Ok(entries) => entries,
        Err(error) => return archive_failure_for("tar", &error),
    };
    let selected = match select_tar_entries(&entries, &parsed.paths) {
        Ok(selected) => selected,
        Err(error) => return archive_failure_for("tar", &error),
    };
    entry_names_at(&entries, &selected)
}

fn tar_extract(context: &mut CommandContext<'_>, parsed: &TarArguments<'_>) -> CommandOutput {
    let destination = parsed.destination.as_deref().unwrap_or(".");
    let archive = match context.fs.read(parsed.archive) {
        Ok(bytes) => bytes,
        Err(error) => return fs_failure("tar", &error),
    };
    let entries = match read_tar_entries(&archive, parsed.gzip) {
        Ok(entries) => entries,
        Err(error) => return archive_failure_for("tar", &error),
    };
    let selected = match select_tar_entries(&entries, &parsed.paths) {
        Ok(selected) => selected,
        Err(error) => return archive_failure_for("tar", &error),
    };
    if let Err(error) = context.fs.make_directory(destination, true) {
        return fs_failure("tar", &error);
    }
    for index in &selected {
        let entry = &entries[*index];
        if let Some(output) = context.take_cancellation() {
            return output;
        }
        let output_path = append_path(destination, &entry.name);
        if entry.directory {
            if let Err(error) = context.fs.make_directory(&output_path, true) {
                return fs_failure("tar", &error);
            }
            continue;
        }
        let parent =
            output_path.rsplit_once('/').map_or(
                ".",
                |(parent, _)| {
                    if parent.is_empty() {
                        "/"
                    } else {
                        parent
                    }
                },
            );
        if let Err(error) = context.fs.make_directory(parent, true) {
            return fs_failure("tar", &error);
        }
        if let Err(error) = context.fs.write(&output_path, &entry.bytes, false) {
            return fs_failure("tar", &error);
        }
    }
    if parsed.verbose {
        return entry_names_at(&entries, &selected);
    }
    CommandOutput::success(format!(
        "extracted {} entries into {destination}\n",
        selected.len()
    ))
}

fn entry_names(entries: &[ArchiveEntry]) -> CommandOutput {
    let mut output = String::new();
    for entry in entries {
        output.push_str(&entry.name);
        output.push('\n');
    }
    CommandOutput::success(output)
}

fn entry_names_at(entries: &[ArchiveEntry], indices: &[usize]) -> CommandOutput {
    let mut output = String::new();
    for index in indices {
        output.push_str(&entries[*index].name);
        output.push('\n');
    }
    CommandOutput::success(output)
}

fn select_tar_entries(entries: &[ArchiveEntry], filters: &[&str]) -> Result<Vec<usize>, String> {
    if filters.is_empty() {
        return Ok((0..entries.len()).collect());
    }
    let normalized = filters
        .iter()
        .map(|filter| {
            validate_archive_name(filter)?;
            Ok(filter.trim_end_matches('/'))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (filter, normalized_filter) in filters.iter().zip(&normalized) {
        let prefix = format!("{normalized_filter}/");
        if !entries
            .iter()
            .any(|entry| entry.name == *normalized_filter || entry.name.starts_with(&prefix))
        {
            return Err(format!("tar member not found: {filter}"));
        }
    }
    Ok(entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            normalized
                .iter()
                .any(|filter| {
                    entry.name == *filter || entry.name.starts_with(&format!("{filter}/"))
                })
                .then_some(index)
        })
        .collect())
}

fn select_zip_entries(entries: &[ArchiveEntry], filters: &[&str]) -> Result<Vec<usize>, String> {
    if filters.is_empty() {
        return Ok((0..entries.len()).collect());
    }
    let normalized = filters
        .iter()
        .map(|filter| {
            validate_archive_name(filter)?;
            Ok(filter.trim_end_matches('/'))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (filter, normalized_filter) in filters.iter().zip(&normalized) {
        let prefix = format!("{normalized_filter}/");
        if !entries
            .iter()
            .any(|entry| entry.name == *normalized_filter || entry.name.starts_with(&prefix))
        {
            return Err(format!("unzip member not found: {filter}"));
        }
    }
    Ok(entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            normalized
                .iter()
                .any(|filter| {
                    entry.name == *filter || entry.name.starts_with(&format!("{filter}/"))
                })
                .then_some(index)
        })
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TarMode {
    Create,
    List,
    Extract,
}

#[derive(Debug)]
struct TarArguments<'a> {
    mode: TarMode,
    archive: &'a str,
    destination: Option<String>,
    paths: Vec<&'a str>,
    verbose: bool,
    gzip: bool,
}

fn parse_tar_arguments(arguments: &[String]) -> Result<TarArguments<'_>, String> {
    TarParser::new(arguments).parse()
}

struct TarParser<'a> {
    arguments: &'a [String],
    mode: Option<TarMode>,
    archive: Option<&'a str>,
    destination: Option<String>,
    paths: Vec<&'a str>,
    verbose: bool,
    gzip: bool,
}

impl<'a> TarParser<'a> {
    fn new(arguments: &'a [String]) -> Self {
        Self {
            arguments,
            mode: None,
            archive: None,
            destination: None,
            paths: Vec::new(),
            verbose: false,
            gzip: false,
        }
    }

    fn parse(self) -> Result<TarArguments<'a>, String> {
        let mut parser = self;
        let mut index = 0;
        let mut parse_options = true;
        while index < parser.arguments.len() {
            let argument = parser.arguments[index].as_str();
            if parse_options && argument == "--" {
                parse_options = false;
            } else if !parse_options {
                parser.paths.push(argument);
            } else if parser.parse_long_option(argument, &mut index)?
                || parser.parse_short_options(argument, &mut index)?
            {
                // The option parser consumed any associated argument.
            } else {
                parser.paths.push(argument);
            }
            index += 1;
        }
        let mode = parser.mode.ok_or("tar requires one of -c, -t, or -x")?;
        let archive = parser.archive.ok_or("tar requires -f ARCHIVE")?;
        Ok(TarArguments {
            mode,
            archive,
            destination: parser.destination,
            paths: parser.paths,
            verbose: parser.verbose,
            gzip: parser.gzip,
        })
    }

    fn parse_long_option(&mut self, argument: &'a str, index: &mut usize) -> Result<bool, String> {
        if argument == "-C" || argument == "--directory" {
            *index += 1;
            let value = self
                .arguments
                .get(*index)
                .ok_or("-C requires a destination")?
                .as_str();
            if value.is_empty() {
                return Err("-C requires a non-empty destination".to_string());
            }
            self.destination = Some(value.to_string());
            return Ok(true);
        }
        if let Some(value) = argument.strip_prefix("--file=") {
            self.set_archive(value)?;
            return Ok(true);
        }
        if argument == "--file" {
            *index += 1;
            let value = self
                .arguments
                .get(*index)
                .ok_or("--file requires an archive path")?
                .as_str();
            self.set_archive(value)?;
            return Ok(true);
        }
        let mode = match argument {
            "--create" => Some(TarMode::Create),
            "--list" => Some(TarMode::List),
            "--extract" => Some(TarMode::Extract),
            _ => None,
        };
        if let Some(mode) = mode {
            set_tar_mode(&mut self.mode, mode)?;
            return Ok(true);
        }
        if argument == "--gzip" {
            self.gzip = true;
            return Ok(true);
        }
        Ok(false)
    }

    fn parse_short_options(
        &mut self,
        argument: &'a str,
        index: &mut usize,
    ) -> Result<bool, String> {
        if !argument.starts_with('-') || argument.len() <= 1 {
            return Ok(false);
        }
        let flags = &argument[1..];
        let mut flag_index = 0;
        while flag_index < flags.len() {
            let flag = flags.as_bytes()[flag_index] as char;
            match flag {
                'c' => set_tar_mode(&mut self.mode, TarMode::Create)?,
                't' => set_tar_mode(&mut self.mode, TarMode::List)?,
                'x' => set_tar_mode(&mut self.mode, TarMode::Extract)?,
                'v' => self.verbose = true,
                'z' => self.gzip = true,
                'j' | 'J' => {
                    return Err("only gzip-compressed tar archives are supported".to_string())
                }
                'f' => {
                    let inline = &flags[flag_index + 1..];
                    if inline.is_empty() {
                        *index += 1;
                        let value = self
                            .arguments
                            .get(*index)
                            .ok_or("-f requires an archive path")?
                            .as_str();
                        self.set_archive(value)?;
                    } else {
                        self.set_archive(inline)?;
                        break;
                    }
                }
                _ => return Err(format!("unsupported tar option: -{flag}")),
            }
            flag_index += 1;
        }
        Ok(true)
    }

    fn set_archive(&mut self, value: &'a str) -> Result<(), String> {
        if value.is_empty() || self.archive.is_some() {
            return Err("tar accepts exactly one archive path".to_string());
        }
        self.archive = Some(value);
        Ok(())
    }
}

fn set_tar_mode(mode: &mut Option<TarMode>, next: TarMode) -> Result<(), String> {
    if mode.replace(next).is_some_and(|current| current != next) {
        return Err("tar accepts exactly one operation mode".to_string());
    }
    Ok(())
}

fn collect_entries(
    context: &mut CommandContext<'_>,
    source_path: &str,
    archive_name: &str,
    recursive: bool,
    entries: &mut Vec<ArchiveEntry>,
    command: &str,
) -> Result<(), CommandOutput> {
    if let Some(output) = context.take_cancellation() {
        return Err(output);
    }
    if entries.len() >= MAX_ARCHIVE_ENTRIES {
        return Err(archive_failure_for(command, "entry limit exceeded"));
    }
    if !archive_name.is_empty() {
        validate_archive_name(archive_name)
            .map_err(|error| archive_failure_for(command, &error))?;
    }
    let info = context
        .fs
        .metadata(source_path)
        .map_err(|error| fs_failure(command, &error))?;
    if info.is_symlink {
        return Err(archive_failure_for(
            command,
            "symbolic links are not archived",
        ));
    }
    if info.is_directory {
        if !recursive {
            return Err(archive_failure_for(
                command,
                &format!("{source_path} is a directory; use -r to recurse"),
            ));
        }
        if !archive_name.is_empty() {
            entries.push(ArchiveEntry {
                name: format!("{}/", archive_name.trim_end_matches('/')),
                bytes: Vec::new(),
                directory: true,
            });
        }
        let children = context
            .fs
            .list(Some(source_path))
            .map_err(|error| fs_failure(command, &error))?;
        for child in children {
            let child_source = append_path(source_path, &child.name);
            let child_name = if archive_name.is_empty() {
                child.name.clone()
            } else {
                append_path(archive_name.trim_end_matches('/'), &child.name)
            };
            collect_entries(
                context,
                &child_source,
                &child_name,
                recursive,
                entries,
                command,
            )?;
        }
        return Ok(());
    }
    let bytes = context
        .fs
        .read(source_path)
        .map_err(|error| fs_failure(command, &error))?;
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(archive_failure_for(
            command,
            "archive payload exceeds the 64 MiB limit",
        ));
    }
    entries.push(ArchiveEntry {
        name: archive_name.to_string(),
        bytes,
        directory: false,
    });
    let total = entries.iter().map(|entry| entry.bytes.len()).sum::<usize>();
    if total > MAX_ARCHIVE_BYTES {
        return Err(archive_failure_for(
            command,
            "archive payload exceeds the 64 MiB limit",
        ));
    }
    Ok(())
}

fn build_tar_archive(entries: &[ArchiveEntry]) -> Result<Vec<u8>, String> {
    if entries.len() > MAX_ARCHIVE_ENTRIES {
        return Err("entry limit exceeded".to_string());
    }
    let mut output = Vec::new();
    for entry in entries {
        let name = if entry.directory {
            format!("{}/", entry.name.trim_end_matches('/'))
        } else {
            entry.name.clone()
        };
        validate_archive_name(&name)?;
        let (name_field, prefix_field) = split_ustar_name(&name)?;
        let mut header = [0_u8; 512];
        write_tar_string(&mut header[0..100], name_field.as_bytes())?;
        write_tar_octal(&mut header[100..108], 0o777)?;
        write_tar_octal(&mut header[108..116], 0)?;
        write_tar_octal(&mut header[116..124], 0)?;
        write_tar_octal(&mut header[124..136], entry.bytes.len() as u64)?;
        write_tar_octal(&mut header[136..148], 0)?;
        header[156] = if entry.directory { b'5' } else { b'0' };
        write_tar_string(&mut header[257..263], b"ustar\0")?;
        write_tar_string(&mut header[263..265], b"00")?;
        write_tar_string(&mut header[265..297], b"rune")?;
        write_tar_string(&mut header[297..329], b"rune")?;
        write_tar_string(&mut header[345..500], prefix_field.as_bytes())?;
        header[148..156].fill(b' ');
        let checksum = header.iter().map(|byte| u64::from(*byte)).sum::<u64>();
        write_tar_checksum(&mut header[148..156], checksum)?;
        output.extend_from_slice(&header);
        if !entry.directory {
            output.extend_from_slice(&entry.bytes);
            let padding = (512 - (entry.bytes.len() % 512)) % 512;
            output.resize(output.len() + padding, 0);
        }
        if output.len() > MAX_ARCHIVE_BYTES.saturating_sub(1024) {
            return Err("archive exceeds the 64 MiB limit".to_string());
        }
    }
    output.resize(output.len() + 1024, 0);
    if output.len() > MAX_ARCHIVE_BYTES {
        return Err("archive exceeds the 64 MiB limit".to_string());
    }
    Ok(output)
}

fn compress_tar_archive(archive: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(archive)
        .map_err(|error| format!("gzip compression failed: {error}"))?;
    let compressed = encoder
        .finish()
        .map_err(|error| format!("gzip compression failed: {error}"))?;
    if compressed.len() > MAX_ARCHIVE_BYTES {
        return Err("compressed archive exceeds the 64 MiB limit".to_string());
    }
    Ok(compressed)
}

fn read_tar_entries(archive: &[u8], gzip: bool) -> Result<Vec<ArchiveEntry>, String> {
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err("archive exceeds the 64 MiB limit".to_string());
    }
    if !gzip {
        return read_tar_archive(archive);
    }
    let decoder = GzDecoder::new(archive);
    let mut bounded = decoder.take((MAX_ARCHIVE_BYTES + 1) as u64);
    let mut decompressed = Vec::new();
    bounded
        .read_to_end(&mut decompressed)
        .map_err(|error| format!("gzip decompression failed: {error}"))?;
    if decompressed.len() > MAX_ARCHIVE_BYTES {
        return Err("decompressed archive exceeds the 64 MiB limit".to_string());
    }
    read_tar_archive(&decompressed)
}

fn read_tar_archive(archive: &[u8]) -> Result<Vec<ArchiveEntry>, String> {
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err("archive exceeds the 64 MiB limit".to_string());
    }
    if archive.len() % 512 != 0 {
        return Err("USTAR archive is not aligned to 512-byte blocks".to_string());
    }
    let mut cursor = 0;
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    while cursor + 512 <= archive.len() {
        let header = &archive[cursor..cursor + 512];
        if header.iter().all(|byte| *byte == 0) {
            if archive[cursor..].iter().any(|byte| *byte != 0) {
                return Err("data follows the USTAR end marker".to_string());
            }
            return if entries.len() <= MAX_ARCHIVE_ENTRIES {
                Ok(entries)
            } else {
                Err("entry limit exceeded".to_string())
            };
        }
        let stored_checksum = read_tar_octal(&header[148..156])?;
        let calculated_checksum = header
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                if (148..156).contains(&index) {
                    u64::from(b' ')
                } else {
                    u64::from(*byte)
                }
            })
            .sum::<u64>();
        if stored_checksum != calculated_checksum {
            return Err("USTAR header checksum mismatch".to_string());
        }
        let name = tar_header_name(header)?;
        validate_archive_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(format!("duplicate USTAR entry: {name}"));
        }
        let size = usize::try_from(read_tar_octal(&header[124..136])?)
            .map_err(|_| "USTAR entry is too large for this platform")?;
        let directory = match header[156] {
            0 | b'0' => false,
            b'5' => {
                if size != 0 {
                    return Err(format!("directory entry has data: {name}"));
                }
                true
            }
            other => {
                return Err(format!(
                    "unsupported USTAR entry type 0x{other:02x}: {name}"
                ))
            }
        };
        let data_start = cursor + 512;
        let data_end = data_start
            .checked_add(size)
            .ok_or("USTAR entry size overflows")?;
        let padded_size = size
            .checked_add(511)
            .ok_or("USTAR entry padding overflows")?
            / 512
            * 512;
        let next = data_start
            .checked_add(padded_size)
            .ok_or("USTAR entry boundary overflows")?;
        if data_end > archive.len() || next > archive.len() {
            return Err(format!("truncated USTAR entry: {name}"));
        }
        entries.push(ArchiveEntry {
            name,
            bytes: if directory {
                Vec::new()
            } else {
                archive[data_start..data_end].to_vec()
            },
            directory,
        });
        if entries.len() > MAX_ARCHIVE_ENTRIES {
            return Err("entry limit exceeded".to_string());
        }
        cursor = next;
    }
    Err("USTAR end marker is missing".to_string())
}

fn split_ustar_name(name: &str) -> Result<(String, String), String> {
    if name.len() <= 100 {
        return Ok((name.to_string(), String::new()));
    }
    for (index, character) in name.char_indices().rev() {
        if character != '/' || index == 0 {
            continue;
        }
        let prefix = &name[..index];
        let suffix = &name[index + 1..];
        if prefix.len() <= 155 && suffix.len() <= 100 {
            return Ok((suffix.to_string(), prefix.to_string()));
        }
    }
    Err("path is too long for the USTAR name fields".to_string())
}

fn tar_header_name(header: &[u8]) -> Result<String, String> {
    let name = tar_string(&header[0..100])?;
    let prefix = tar_string(&header[345..500])?;
    if prefix.is_empty() {
        Ok(name)
    } else if name.is_empty() {
        Ok(prefix)
    } else {
        Ok(format!("{prefix}/{name}"))
    }
}

fn tar_string(field: &[u8]) -> Result<String, String> {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    std::str::from_utf8(&field[..end])
        .map(str::to_string)
        .map_err(|_| "USTAR header contains a non-UTF-8 path".to_string())
}

fn write_tar_string(field: &mut [u8], value: &[u8]) -> Result<(), String> {
    if value.len() > field.len() {
        return Err("USTAR header field is too small".to_string());
    }
    field[..value.len()].copy_from_slice(value);
    Ok(())
}

fn write_tar_octal(field: &mut [u8], value: u64) -> Result<(), String> {
    let digits = format!("{value:o}");
    if digits.len() + 1 > field.len() {
        return Err("USTAR numeric field is too small".to_string());
    }
    field.fill(b'0');
    let start = field.len() - digits.len() - 1;
    field[start..start + digits.len()].copy_from_slice(digits.as_bytes());
    field[field.len() - 1] = 0;
    Ok(())
}

fn write_tar_checksum(field: &mut [u8], value: u64) -> Result<(), String> {
    let digits = format!("{value:o}");
    if digits.len() + 2 > field.len() {
        return Err("USTAR checksum field is too small".to_string());
    }
    field.fill(0);
    let start = field.len() - digits.len() - 2;
    field[start..start + digits.len()].copy_from_slice(digits.as_bytes());
    field[field.len() - 2] = 0;
    field[field.len() - 1] = b' ';
    Ok(())
}

fn read_tar_octal(field: &[u8]) -> Result<u64, String> {
    let trimmed = field
        .iter()
        .copied()
        .skip_while(|byte| *byte == 0 || *byte == b' ')
        .take_while(|byte| *byte != 0 && *byte != b' ')
        .collect::<Vec<_>>();
    if trimmed.is_empty() {
        return Ok(0);
    }
    if trimmed.iter().any(|byte| !matches!(byte, b'0'..=b'7')) {
        return Err("invalid USTAR octal field".to_string());
    }
    let text =
        std::str::from_utf8(&trimmed).map_err(|_| "invalid USTAR numeric field".to_string())?;
    u64::from_str_radix(text, 8).map_err(|_| "invalid USTAR numeric field".to_string())
}

fn build_archive(entries: &[ArchiveEntry]) -> Result<Vec<u8>, String> {
    if entries.len() > MAX_ARCHIVE_ENTRIES {
        return Err("entry limit exceeded".to_string());
    }
    let mut output = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        validate_archive_name(&entry.name)?;
        let name = entry.name.as_bytes();
        let name_length = u16::try_from(name.len()).map_err(|_| "file name is too long")?;
        let size = u32::try_from(entry.bytes.len()).map_err(|_| "entry is too large")?;
        let (method, compressed) = if entry.directory {
            (0, Vec::new())
        } else {
            compress_zip_entry(&entry.bytes)?
        };
        let compressed_size =
            u32::try_from(compressed.len()).map_err(|_| "compressed entry is too large")?;
        let offset = u32::try_from(output.len()).map_err(|_| "archive is too large")?;
        let crc = crc32(&entry.bytes);
        push_u32(&mut output, 0x0403_4b50);
        push_u16(&mut output, 20);
        push_u16(&mut output, 0);
        push_u16(&mut output, method);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0);
        push_u32(&mut output, crc);
        push_u32(&mut output, compressed_size);
        push_u32(&mut output, size);
        push_u16(&mut output, name_length);
        push_u16(&mut output, 0);
        output.extend_from_slice(name);
        output.extend_from_slice(&compressed);

        push_u32(&mut central, 0x0201_4b50);
        push_u16(&mut central, 20);
        push_u16(&mut central, 20);
        push_u16(&mut central, 0);
        push_u16(&mut central, method);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, crc);
        push_u32(&mut central, compressed_size);
        push_u32(&mut central, size);
        push_u16(&mut central, name_length);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, if entry.directory { 0x10 } else { 0 });
        push_u32(&mut central, offset);
        central.extend_from_slice(name);
    }
    let central_offset = u32::try_from(output.len()).map_err(|_| "archive is too large")?;
    let central_size = u32::try_from(central.len()).map_err(|_| "archive is too large")?;
    output.extend_from_slice(&central);
    let count = u16::try_from(entries.len()).map_err(|_| "too many archive entries")?;
    push_u32(&mut output, 0x0605_4b50);
    push_u16(&mut output, 0);
    push_u16(&mut output, 0);
    push_u16(&mut output, count);
    push_u16(&mut output, count);
    push_u32(&mut output, central_size);
    push_u32(&mut output, central_offset);
    push_u16(&mut output, 0);
    if output.len() > MAX_ARCHIVE_BYTES {
        return Err("archive exceeds the 64 MiB limit".to_string());
    }
    Ok(output)
}

fn compress_zip_entry(bytes: &[u8]) -> Result<(u16, Vec<u8>), String> {
    if bytes.is_empty() {
        return Ok((0, Vec::new()));
    }
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(bytes)
        .map_err(|error| format!("ZIP compression failed: {error}"))?;
    let compressed = encoder
        .finish()
        .map_err(|error| format!("ZIP compression failed: {error}"))?;
    if compressed.len() < bytes.len() {
        Ok((8, compressed))
    } else {
        Ok((0, bytes.to_vec()))
    }
}

fn read_central_directory(archive: &[u8]) -> Result<Vec<ArchiveEntry>, String> {
    let eocd = find_end_of_central_directory(archive).ok_or("end record is missing")?;
    if eocd + 22 > archive.len() {
        return Err("end record is truncated".to_string());
    }
    let disk = read_u16(archive, eocd + 4)?;
    let central_disk = read_u16(archive, eocd + 6)?;
    let entries_on_disk = usize::from(read_u16(archive, eocd + 8)?);
    let entries_total = usize::from(read_u16(archive, eocd + 10)?);
    let central_size = usize_from_u32(read_u32(archive, eocd + 12)?)?;
    let central_offset = usize_from_u32(read_u32(archive, eocd + 16)?)?;
    if disk != 0 || central_disk != 0 || entries_on_disk != entries_total {
        return Err("multi-disk ZIP archives are not supported".to_string());
    }
    if entries_total > MAX_ARCHIVE_ENTRIES {
        return Err("entry limit exceeded".to_string());
    }
    let central_end = central_offset
        .checked_add(central_size)
        .ok_or("central directory size overflows")?;
    if central_end > archive.len() || central_end > eocd {
        return Err("central directory is outside the archive".to_string());
    }

    let mut cursor = central_offset;
    let mut entries = Vec::with_capacity(entries_total);
    let mut names = BTreeSet::new();
    let mut uncompressed_total = 0_usize;
    for _ in 0..entries_total {
        if cursor + 46 > central_end || read_u32(archive, cursor)? != 0x0201_4b50 {
            return Err("central directory entry is malformed".to_string());
        }
        let flags = read_u16(archive, cursor + 8)?;
        let method = read_u16(archive, cursor + 10)?;
        let crc = read_u32(archive, cursor + 16)?;
        let compressed_size = usize_from_u32(read_u32(archive, cursor + 20)?)?;
        let uncompressed_size = usize_from_u32(read_u32(archive, cursor + 24)?)?;
        let name_length = usize::from(read_u16(archive, cursor + 28)?);
        let extra_length = usize::from(read_u16(archive, cursor + 30)?);
        let comment_length = usize::from(read_u16(archive, cursor + 32)?);
        let local_offset = usize_from_u32(read_u32(archive, cursor + 42)?)?;
        let name_start = cursor + 46;
        let name_end = name_start
            .checked_add(name_length)
            .ok_or("file name length overflows")?;
        let next = name_end
            .checked_add(extra_length)
            .and_then(|value| value.checked_add(comment_length))
            .ok_or("central entry length overflows")?;
        if next > central_end {
            return Err("central directory entry is truncated".to_string());
        }
        let name = std::str::from_utf8(&archive[name_start..name_end])
            .map_err(|_| "central entry name is not UTF-8".to_string())?
            .to_string();
        validate_archive_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(format!("duplicate ZIP entry: {name}"));
        }
        if flags & !ZIP_ALLOWED_FLAGS != 0
            || !matches!(method, 0 | 8)
            || (method == 0 && compressed_size != uncompressed_size)
        {
            return Err(format!("unsupported ZIP entry: {name}"));
        }
        uncompressed_total = uncompressed_total
            .checked_add(uncompressed_size)
            .ok_or("uncompressed ZIP size overflows")?;
        if uncompressed_total > MAX_ARCHIVE_BYTES {
            return Err("uncompressed ZIP payload exceeds the 64 MiB limit".to_string());
        }
        let bytes = read_local_entry(
            archive,
            local_offset,
            &name,
            ZipEntryMetadata {
                flags,
                method,
                compressed_size,
                uncompressed_size,
                crc,
            },
        )?;
        entries.push(ArchiveEntry {
            directory: name.ends_with('/'),
            name,
            bytes,
        });
        cursor = next;
    }
    Ok(entries)
}

fn read_local_entry(
    archive: &[u8],
    offset: usize,
    name: &str,
    metadata: ZipEntryMetadata,
) -> Result<Vec<u8>, String> {
    if offset + 30 > archive.len() || read_u32(archive, offset)? != 0x0403_4b50 {
        return Err(format!("local entry is malformed: {name}"));
    }
    if read_u16(archive, offset + 6)? != metadata.flags
        || read_u16(archive, offset + 8)? != metadata.method
    {
        return Err(format!("unsupported local entry: {name}"));
    }
    let name_length = usize::from(read_u16(archive, offset + 26)?);
    let extra_length = usize::from(read_u16(archive, offset + 28)?);
    let data_start = offset
        .checked_add(30)
        .and_then(|value| value.checked_add(name_length))
        .and_then(|value| value.checked_add(extra_length))
        .ok_or("local entry length overflows")?;
    let data_end = data_start
        .checked_add(metadata.compressed_size)
        .ok_or("local entry size overflows")?;
    let local_name = std::str::from_utf8(&archive[offset + 30..offset + 30 + name_length])
        .map_err(|_| "local entry name is not UTF-8".to_string())?;
    if data_end > archive.len() || local_name != name {
        return Err(format!(
            "local entry does not match central directory: {name}"
        ));
    }
    if metadata.flags & ZIP_DATA_DESCRIPTOR_FLAG != 0 {
        validate_zip_data_descriptor(archive, data_end, metadata, name)?;
    }
    let compressed = &archive[data_start..data_end];
    let bytes = match metadata.method {
        0 => compressed.to_vec(),
        8 => decompress_zip_entry(compressed, metadata.uncompressed_size, name)?,
        _ => return Err(format!("unsupported ZIP entry: {name}")),
    };
    if bytes.len() != metadata.uncompressed_size {
        return Err(format!("uncompressed size mismatch: {name}"));
    }
    if crc32(&bytes) != metadata.crc {
        return Err(format!("CRC mismatch: {name}"));
    }
    Ok(bytes)
}

fn validate_zip_data_descriptor(
    archive: &[u8],
    offset: usize,
    metadata: ZipEntryMetadata,
    name: &str,
) -> Result<(), String> {
    let signature = read_u32(archive, offset)
        .map_err(|_| format!("ZIP data descriptor is truncated: {name}"))?;
    let fields = if signature == ZIP_DATA_DESCRIPTOR_SIGNATURE {
        offset
            .checked_add(4)
            .ok_or_else(|| format!("ZIP data descriptor overflows: {name}"))?
    } else {
        offset
    };
    let descriptor_length = if signature == ZIP_DATA_DESCRIPTOR_SIGNATURE {
        16
    } else {
        12
    };
    let descriptor_end = offset
        .checked_add(descriptor_length)
        .ok_or_else(|| format!("ZIP data descriptor overflows: {name}"))?;
    if descriptor_end > archive.len() {
        return Err(format!("ZIP data descriptor is truncated: {name}"));
    }
    let crc = read_u32(archive, fields)
        .map_err(|_| format!("ZIP data descriptor is truncated: {name}"))?;
    let compressed_size = usize_from_u32(read_u32(archive, fields + 4)?)?;
    let uncompressed_size = usize_from_u32(read_u32(archive, fields + 8)?)?;
    if crc != metadata.crc
        || compressed_size != metadata.compressed_size
        || uncompressed_size != metadata.uncompressed_size
    {
        return Err(format!(
            "ZIP data descriptor does not match central directory: {name}"
        ));
    }
    Ok(())
}

fn decompress_zip_entry(
    compressed: &[u8],
    expected_size: usize,
    name: &str,
) -> Result<Vec<u8>, String> {
    let mut decoder = DeflateDecoder::new(compressed);
    let mut bytes = Vec::with_capacity(expected_size);
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = decoder
            .read(&mut buffer)
            .map_err(|error| format!("ZIP decompression failed for {name}: {error}"))?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > expected_size {
            return Err(format!("uncompressed size exceeds ZIP header: {name}"));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    Ok(bytes)
}

fn validate_archive_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_ARCHIVE_PATH_BYTES || name.starts_with('/') {
        return Err("archive path is empty, absolute, or too long".to_string());
    }
    if name.contains('\\') {
        return Err("archive paths cannot contain backslashes".to_string());
    }
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty()
        || trimmed
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(format!("unsafe archive path: {name}"));
    }
    Ok(())
}

fn archive_name(path: &str) -> String {
    path.strip_prefix("~/")
        .or_else(|| path.strip_prefix("./"))
        .unwrap_or(path)
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}

fn append_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else if parent.ends_with('/') {
        format!("{parent}{child}")
    } else {
        format!("{parent}/{child}")
    }
}

fn archive_failure(message: &str) -> CommandOutput {
    archive_failure_for("zip/unzip", message)
}

fn archive_failure_for(command: &str, message: &str) -> CommandOutput {
    CommandOutput::failure(1, format!("{command}: {message}\n"))
}

fn find_end_of_central_directory(bytes: &[u8]) -> Option<usize> {
    let start = bytes.len().saturating_sub(65_557);
    (start..bytes.len().saturating_sub(21))
        .rev()
        .find(|&index| {
            read_u32(bytes, index).ok() == Some(0x0605_4b50)
                && read_u16(bytes, index + 20)
                    .ok()
                    .and_then(|comment_length| index.checked_add(22 + usize::from(comment_length)))
                    == Some(bytes.len())
        })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or("archive record is truncated")?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or("archive record is truncated")?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn usize_from_u32(value: u32) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| "archive size does not fit this platform".to_string())
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::{
        build_archive, build_tar_archive, crc32, push_u32, read_central_directory,
        read_tar_archive, read_u16, read_u32, validate_archive_name, ArchiveEntry,
        ZIP_DATA_DESCRIPTOR_FLAG, ZIP_DATA_DESCRIPTOR_SIGNATURE,
    };

    #[test]
    fn rejects_archive_names_that_can_escape_on_extraction() {
        for name in [
            "/absolute",
            "../outside",
            "safe/../outside",
            "safe\\outside",
        ] {
            assert!(
                validate_archive_name(name).is_err(),
                "unsafe archive name accepted: {name}"
            );
        }
        assert!(validate_archive_name("safe/note.txt").is_ok());
        assert!(validate_archive_name("safe/").is_ok());
    }

    #[test]
    fn computes_the_standard_crc32_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn deflates_repetitive_zip_entries_and_round_trips_them() {
        let entry = ArchiveEntry {
            name: "repeated.txt".to_string(),
            bytes: b"repeated content ".repeat(256),
            directory: false,
        };
        let archive = build_archive(&[entry]).expect("ZIP archive built");
        assert_eq!(read_u16(&archive, 8).expect("local method present"), 8);
        let entries = read_central_directory(&archive).expect("ZIP archive read");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].bytes, b"repeated content ".repeat(256));
    }

    #[test]
    fn reads_zip_entries_with_signature_data_descriptors() {
        let entry = ArchiveEntry {
            name: "descriptor.txt".to_string(),
            bytes: b"descriptor content ".repeat(256),
            directory: false,
        };
        let archive = build_archive(&[entry]).expect("ZIP archive built");
        let eocd = archive.len() - 22;
        let central_offset =
            usize::try_from(read_u32(&archive, eocd + 16).expect("central offset present"))
                .expect("central offset fits");
        let name_length = usize::from(read_u16(&archive, 26).expect("local name length present"));
        let extra_length = usize::from(read_u16(&archive, 28).expect("local extra length present"));
        let compressed_size =
            usize::try_from(read_u32(&archive, 18).expect("local compressed size present"))
                .expect("compressed size fits");
        let data_end = 30 + name_length + extra_length + compressed_size;
        let crc = read_u32(&archive, 14).expect("local CRC present");
        let uncompressed_size = read_u32(&archive, 22).expect("local size present");

        let mut descriptor_archive = archive[..data_end].to_vec();
        descriptor_archive[6..8].copy_from_slice(&ZIP_DATA_DESCRIPTOR_FLAG.to_le_bytes());
        descriptor_archive[14..18].fill(0);
        descriptor_archive[18..22].fill(0);
        descriptor_archive[22..26].fill(0);
        push_u32(&mut descriptor_archive, ZIP_DATA_DESCRIPTOR_SIGNATURE);
        push_u32(&mut descriptor_archive, crc);
        push_u32(
            &mut descriptor_archive,
            u32::try_from(compressed_size).expect("compressed size fits ZIP32"),
        );
        push_u32(&mut descriptor_archive, uncompressed_size);
        let new_central_offset_u32 =
            u32::try_from(descriptor_archive.len()).expect("central offset fits ZIP32");
        descriptor_archive.extend_from_slice(&archive[central_offset..]);
        let new_central_offset = usize::try_from(new_central_offset_u32).expect("offset fits");
        descriptor_archive[new_central_offset + 8..new_central_offset + 10]
            .copy_from_slice(&ZIP_DATA_DESCRIPTOR_FLAG.to_le_bytes());
        let new_eocd = descriptor_archive.len() - 22;
        descriptor_archive[new_eocd + 16..new_eocd + 20]
            .copy_from_slice(&new_central_offset_u32.to_le_bytes());

        let entries = read_central_directory(&descriptor_archive).expect("ZIP archive read");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].bytes, b"descriptor content ".repeat(256));

        descriptor_archive[data_end + 4..data_end + 8].copy_from_slice(&0_u32.to_le_bytes());
        assert!(read_central_directory(&descriptor_archive)
            .expect_err("mismatched data descriptor must be rejected")
            .contains("data descriptor does not match central directory"));
    }

    #[test]
    fn rejects_unsafe_or_link_ustar_members() {
        let entry = ArchiveEntry {
            name: "safe.txt".to_string(),
            bytes: b"safe".to_vec(),
            directory: false,
        };
        let mut archive = build_tar_archive(&[entry]).expect("test archive built");
        archive[0..100].fill(0);
        archive[0..9].copy_from_slice(b"../escape");
        archive[148..156].fill(b' ');
        let checksum = archive[0..512]
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                if (148..156).contains(&index) {
                    u64::from(b' ')
                } else {
                    u64::from(*byte)
                }
            })
            .sum::<u64>();
        super::write_tar_checksum(&mut archive[148..156], checksum)
            .expect("checksum fits the header");
        assert!(read_tar_archive(&archive)
            .expect_err("path traversal must be rejected")
            .contains("unsafe archive path"));

        let entry = ArchiveEntry {
            name: "link-target".to_string(),
            bytes: Vec::new(),
            directory: false,
        };
        let mut archive = build_tar_archive(&[entry]).expect("test archive built");
        archive[156] = b'2';
        archive[148..156].fill(b' ');
        let checksum = archive[0..512]
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                if (148..156).contains(&index) {
                    u64::from(b' ')
                } else {
                    u64::from(*byte)
                }
            })
            .sum::<u64>();
        super::write_tar_checksum(&mut archive[148..156], checksum)
            .expect("checksum fits the header");
        assert!(read_tar_archive(&archive)
            .expect_err("links must not cross the VFS boundary")
            .contains("unsupported USTAR entry type"));
    }
}
