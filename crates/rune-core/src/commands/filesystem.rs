use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

const FIND_ENTRY_LIMIT: usize = 10_000;
const TREE_ENTRY_LIMIT: usize = 10_000;
const TREE_OUTPUT_BYTES: usize = 512 * 1024;
const TREE_MAX_DEPTH: usize = 64;

pub(super) fn cd(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() > 1 {
        return usage("cd", "usage: cd [directory]");
    }
    let directory = context.args.first().map_or("~", String::as_str);
    let bookmarked_directory =
        match crate::commands::bookmarks::lookup_cd_bookmark(context, directory) {
            Ok(path) => path,
            Err(output) => return output,
        };
    let directory = match bookmarked_directory.as_deref() {
        Some(path) => path,
        None => directory,
    };
    match context.fs.change_dir(directory) {
        Ok(()) if directory == "-" => {
            CommandOutput::success(format!("{}\n", context.fs.current_dir_display()))
        }
        Ok(()) => CommandOutput::success(""),
        Err(error) => fs_failure("cd", &error),
    }
}

pub(super) fn ls(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut show_hidden = false;
    let mut long_format = false;
    let mut human_sizes = false;
    let mut paths = Vec::new();
    let mut parse_options = true;
    for argument in context.args {
        if parse_options && argument == "--" {
            parse_options = false;
            continue;
        }
        if parse_options {
            if let Some(flags) = argument.strip_prefix('-') {
                if flags.is_empty() {
                    paths.push(argument.as_str());
                } else if flags
                    .chars()
                    .all(|flag| matches!(flag, 'a' | 'A' | 'l' | 'h' | '1'))
                {
                    show_hidden |= flags.contains('a') || flags.contains('A');
                    long_format |= flags.contains('l') || flags.contains('h');
                    human_sizes |= flags.contains('h');
                } else {
                    return usage("ls", "usage: ls [-aAhl1] [--] [path ...]");
                }
            } else {
                paths.push(argument.as_str());
            }
        } else {
            paths.push(argument.as_str());
        }
    }
    if paths.is_empty() {
        paths.push("~");
    }

    let multiple = paths.len() > 1;
    let mut stdout = String::new();
    for (index, path) in paths.iter().enumerate() {
        if multiple {
            if index > 0 {
                stdout.push('\n');
            }
            stdout.push_str(path);
            stdout.push_str(":\n");
        }
        let info = match context.fs.metadata(path) {
            Ok(info) => info,
            Err(error) => return fs_failure("ls", &error),
        };
        if !info.is_directory {
            if show_hidden || !info.name.starts_with('.') {
                append_listing_line(
                    &mut stdout,
                    &info.name,
                    listing_kind(info.is_directory, info.is_symlink),
                    info.size,
                    listing_format(long_format, human_sizes),
                );
            }
            continue;
        }
        let entries = match context.fs.list(Some(path)) {
            Ok(entries) => entries,
            Err(error) => return fs_failure("ls", &error),
        };
        for entry in entries {
            if !show_hidden && entry.name.starts_with('.') {
                continue;
            }
            append_listing_line(
                &mut stdout,
                &entry.name,
                listing_kind(entry.is_directory, entry.is_symlink),
                entry.size,
                listing_format(long_format, human_sizes),
            );
        }
    }
    CommandOutput::success(stdout)
}

#[derive(Clone, Copy)]
enum ListingKind {
    File,
    Directory,
    Symlink,
}

#[derive(Clone, Copy)]
enum ListingFormat {
    Names,
    Long { human_sizes: bool },
}

fn listing_kind(is_directory: bool, is_symlink: bool) -> ListingKind {
    if is_symlink {
        ListingKind::Symlink
    } else if is_directory {
        ListingKind::Directory
    } else {
        ListingKind::File
    }
}

fn listing_format(long_format: bool, human_sizes: bool) -> ListingFormat {
    if long_format {
        ListingFormat::Long { human_sizes }
    } else {
        ListingFormat::Names
    }
}

fn append_listing_line(
    stdout: &mut String,
    name: &str,
    kind: ListingKind,
    size: u64,
    format: ListingFormat,
) {
    let suffix = match kind {
        ListingKind::Directory => "/",
        ListingKind::Symlink => "@",
        ListingKind::File => "",
    };
    if let ListingFormat::Long { human_sizes } = format {
        let marker = match kind {
            ListingKind::Symlink => 'l',
            ListingKind::Directory => 'd',
            ListingKind::File => '-',
        };
        let displayed_size = if human_sizes {
            human_size(size)
        } else {
            size.to_string()
        };
        let _ = writeln!(stdout, "{marker} {displayed_size:>8} {name}{suffix}");
    } else {
        let _ = writeln!(stdout, "{name}{suffix}");
    }
}

fn human_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut unit = 0;
    let mut divisor = 1_u64;
    while size >= divisor.saturating_mul(1024) && unit + 1 < UNITS.len() {
        divisor = divisor.saturating_mul(1024);
        unit += 1;
    }
    if unit == 0 {
        return format!("{size}B");
    }
    let tenths = (size.saturating_mul(10) + divisor / 2) / divisor;
    if tenths >= 100 {
        format!("{}{}", tenths / 10, UNITS[unit])
    } else {
        format!("{}.{}{}", tenths / 10, tenths % 10, UNITS[unit])
    }
}

pub(super) fn cat(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return CommandOutput::success(context.stdin);
    }
    let mut stdout = String::new();
    for path in context.args {
        if path == "-" {
            stdout.push_str(context.stdin);
        } else {
            match context.fs.read(path) {
                Ok(bytes) => stdout.push_str(&String::from_utf8_lossy(&bytes)),
                Err(error) => return fs_failure("cat", &error),
            }
        }
    }
    CommandOutput::success(stdout)
}

pub(super) fn find(context: &mut CommandContext<'_>) -> CommandOutput {
    let (path, options) = match parse_find_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let mut stdout = String::new();
    let mut visited = 0;
    let mut traversal = FindTraversal {
        options: &options,
        visited: &mut visited,
        stdout: &mut stdout,
    };
    if let Err(output) = visit_find(context, &path, 0, &mut traversal) {
        return output;
    }
    CommandOutput::success(stdout)
}

fn parse_find_args(args: &[String]) -> Result<(String, FindOptions), CommandOutput> {
    let mut path = ".".to_string();
    let mut options = FindOptions {
        pattern: None,
        find_type: None,
        min_depth: 0,
        max_depth: None,
    };
    let mut index = 0;
    if let Some(first) = args.first() {
        if !first.starts_with('-') {
            path.clone_from(first);
            index = 1;
        }
    }
    while index < args.len() {
        match args[index].as_str() {
            "--" => {
                index += 1;
                if index >= args.len() {
                    return Err(usage(
                        "find",
                        "usage: find [path] [-name PATTERN] [-type f|d|l] [-mindepth N] [-maxdepth N]",
                    ));
                }
                if index + 1 != args.len() {
                    return Err(usage(
                        "find",
                        "usage: find [path] [-name PATTERN] [-type f|d|l] [-mindepth N] [-maxdepth N]",
                    ));
                }
                path.clone_from(&args[index]);
                index += 1;
            }
            "-name" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(usage("find", "-name requires a pattern"));
                };
                options.pattern = Some(value.clone());
                index += 1;
            }
            "-maxdepth" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(usage("find", "-maxdepth requires a non-negative number"));
                };
                options.max_depth = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| usage("find", "-maxdepth requires a non-negative number"))?,
                );
                index += 1;
            }
            "-mindepth" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(usage("find", "-mindepth requires a non-negative number"));
                };
                options.min_depth = value
                    .parse::<usize>()
                    .map_err(|_| usage("find", "-mindepth requires a non-negative number"))?;
                index += 1;
            }
            "-type" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(usage("find", "-type requires f, d, or l"));
                };
                options.find_type = Some(match value.as_str() {
                    "f" => FindType::File,
                    "d" => FindType::Directory,
                    "l" => FindType::Symlink,
                    _ => return Err(usage("find", "-type requires f, d, or l")),
                });
                index += 1;
            }
            _ => {
                return Err(usage(
                    "find",
                    "usage: find [path] [-name PATTERN] [-type f|d|l] [-mindepth N] [-maxdepth N]",
                ))
            }
        }
    }
    Ok((path, options))
}

#[derive(Clone, Copy)]
enum FindType {
    File,
    Directory,
    Symlink,
}

struct FindOptions {
    pattern: Option<String>,
    find_type: Option<FindType>,
    min_depth: usize,
    max_depth: Option<usize>,
}

struct FindTraversal<'a> {
    options: &'a FindOptions,
    visited: &'a mut usize,
    stdout: &'a mut String,
}

fn visit_find(
    context: &mut CommandContext<'_>,
    path: &str,
    depth: usize,
    traversal: &mut FindTraversal<'_>,
) -> Result<(), CommandOutput> {
    if let Some(output) = context.take_cancellation() {
        return Err(output);
    }
    if *traversal.visited >= FIND_ENTRY_LIMIT {
        return Err(CommandOutput::failure(
            1,
            format!("find: traversal exceeded {FIND_ENTRY_LIMIT} entries\n"),
        ));
    }
    let info = context
        .fs
        .metadata(path)
        .map_err(|error| fs_failure("find", &error))?;
    *traversal.visited += 1;
    let type_matches = match traversal.options.find_type {
        None => true,
        Some(FindType::File) => !info.is_directory && !info.is_symlink,
        Some(FindType::Directory) => info.is_directory && !info.is_symlink,
        Some(FindType::Symlink) => info.is_symlink,
    };
    if depth >= traversal.options.min_depth
        && type_matches
        && traversal
            .options
            .pattern
            .as_deref()
            .map_or(true, |value| wildcard_match(value, path_basename(path)))
    {
        traversal.stdout.push_str(path);
        traversal.stdout.push('\n');
    }
    if !info.is_directory
        || info.is_symlink
        || traversal
            .options
            .max_depth
            .is_some_and(|limit| depth >= limit)
    {
        return Ok(());
    }
    let entries = context
        .fs
        .list(Some(path))
        .map_err(|error| fs_failure("find", &error))?;
    for entry in entries {
        let child = append_child_path(path, &entry.name);
        visit_find(context, &child, depth + 1, traversal)?;
    }
    Ok(())
}

pub(super) fn tree(context: &mut CommandContext<'_>) -> CommandOutput {
    let (options, path) = match parse_tree_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let info = match context.fs.metadata(&path) {
        Ok(info) => info,
        Err(error) => return fs_failure("tree", &error),
    };
    let mut stdout = format!("{path}\n");
    if info.is_directory && !info.is_symlink {
        let mut visited = 1;
        if let Err(output) =
            append_tree_entries(context, &path, "", 0, options, &mut visited, &mut stdout)
        {
            return output;
        }
    }
    CommandOutput::success(stdout)
}

#[derive(Clone, Copy)]
struct TreeOptions {
    show_hidden: bool,
    directories_only: bool,
    max_depth: usize,
}

fn parse_tree_args(args: &[String]) -> Result<(TreeOptions, String), CommandOutput> {
    let mut options = TreeOptions {
        show_hidden: false,
        directories_only: false,
        max_depth: TREE_MAX_DEPTH,
    };
    let mut path = None;
    let mut parse_options = true;
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && matches!(argument.as_str(), "-a" | "--all") {
            options.show_hidden = true;
        } else if parse_options && matches!(argument.as_str(), "-d" | "--dirs-only") {
            options.directories_only = true;
        } else if parse_options && matches!(argument.as_str(), "-L" | "--level") {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err(tree_usage());
            };
            options.max_depth = parse_tree_depth(value)?;
        } else if parse_options && argument.starts_with("-L") {
            options.max_depth = parse_tree_depth(&argument[2..])?;
        } else if parse_options && argument.starts_with("--level=") {
            options.max_depth = parse_tree_depth(&argument[8..])?;
        } else if parse_options && argument.starts_with('-') {
            return Err(tree_usage());
        } else if path.is_none() {
            path = Some(argument.clone());
            parse_options = false;
        } else {
            return Err(tree_usage());
        }
        index += 1;
    }
    Ok((options, path.unwrap_or_else(|| ".".to_string())))
}

fn parse_tree_depth(value: &str) -> Result<usize, CommandOutput> {
    let depth = value
        .parse::<usize>()
        .map_err(|_| usage("tree", "-L/--level requires a positive integer"))?;
    if depth == 0 || depth > TREE_MAX_DEPTH {
        return Err(usage(
            "tree",
            &format!("-L/--level must be between 1 and {TREE_MAX_DEPTH}"),
        ));
    }
    Ok(depth)
}

fn tree_usage() -> CommandOutput {
    usage("tree", "usage: tree [-a] [-d] [-L level] [--] [directory]")
}

fn append_tree_entries(
    context: &mut CommandContext<'_>,
    path: &str,
    indentation: &str,
    depth: usize,
    options: TreeOptions,
    visited: &mut usize,
    stdout: &mut String,
) -> Result<(), CommandOutput> {
    if depth >= options.max_depth {
        return Ok(());
    }
    if let Some(output) = context.take_cancellation() {
        return Err(output);
    }
    let entries = context
        .fs
        .list(Some(path))
        .map_err(|error| fs_failure("tree", &error))?;
    let entries = entries
        .into_iter()
        .filter(|entry| options.show_hidden || !entry.name.starts_with('.'))
        .filter(|entry| !options.directories_only || entry.is_directory)
        .collect::<Vec<_>>();
    for (index, entry) in entries.iter().enumerate() {
        if *visited >= TREE_ENTRY_LIMIT {
            return Err(CommandOutput::failure(
                1,
                format!("tree: traversal exceeded {TREE_ENTRY_LIMIT} entries\n"),
            ));
        }
        *visited += 1;
        let is_last = index + 1 == entries.len();
        let branch = if is_last { "└── " } else { "├── " };
        let suffix = if entry.is_symlink {
            "@"
        } else if entry.is_directory {
            "/"
        } else {
            ""
        };
        let line = format!("{indentation}{branch}{}{suffix}\n", entry.name);
        if stdout.len().saturating_add(line.len()) > TREE_OUTPUT_BYTES {
            return Err(CommandOutput::failure(
                1,
                format!("tree: output exceeds {TREE_OUTPUT_BYTES} bytes\n"),
            ));
        }
        stdout.push_str(&line);
        if entry.is_directory && !entry.is_symlink {
            let next_indentation = if is_last {
                format!("{indentation}    ")
            } else {
                format!("{indentation}│   ")
            };
            let child = append_child_path(path, &entry.name);
            append_tree_entries(
                context,
                &child,
                &next_indentation,
                depth + 1,
                options,
                visited,
                stdout,
            )?;
        }
    }
    Ok(())
}

fn append_child_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else if parent.ends_with('/') {
        format!("{parent}{child}")
    } else {
        format!("{parent}/{child}")
    }
}

fn path_basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for pattern_character in pattern {
        let mut current = vec![false; value.len() + 1];
        if pattern_character == '*' {
            current[0] = previous[0];
            for index in 1..=value.len() {
                current[index] = previous[index] || current[index - 1];
            }
        } else {
            for index in 1..=value.len() {
                current[index] = previous[index - 1]
                    && (pattern_character == '?' || pattern_character == value[index - 1]);
            }
        }
        previous = current;
    }
    previous[value.len()]
}

pub(super) fn mkdir(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut parents = false;
    let mut paths = Vec::new();
    for argument in context.args {
        if let Some(flags) = argument.strip_prefix('-') {
            if flags == "p" {
                parents = true;
            } else {
                return usage("mkdir", "usage: mkdir [-p] directory ...");
            }
        } else {
            paths.push(argument);
        }
    }
    if paths.is_empty() {
        return usage("mkdir", "usage: mkdir [-p] directory ...");
    }
    for path in paths {
        if let Err(error) = context.fs.make_directory(path, parents) {
            return fs_failure("mkdir", &error);
        }
    }
    CommandOutput::success("")
}

pub(super) fn touch(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("touch", "usage: touch file ...");
    }
    for path in context.args {
        if let Err(error) = context.fs.touch(path) {
            return fs_failure("touch", &error);
        }
    }
    CommandOutput::success("")
}

pub(super) fn ln(context: &mut CommandContext<'_>) -> CommandOutput {
    let [flag, target, link] = context.args else {
        return usage("ln", "usage: ln -s TARGET LINK");
    };
    if flag != "-s" && flag != "--symbolic" {
        return usage("ln", "usage: ln -s TARGET LINK");
    }
    context.fs.make_symlink(target, link).map_or_else(
        |error| fs_failure("ln", &error),
        |()| CommandOutput::success(""),
    )
}

pub(super) fn readlink(context: &mut CommandContext<'_>) -> CommandOutput {
    let [path] = context.args else {
        return usage("readlink", "usage: readlink LINK");
    };
    context.fs.read_link(path).map_or_else(
        |error| fs_failure("readlink", &error),
        |target| CommandOutput::success(format!("{target}\n")),
    )
}

pub(super) fn rm(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut recursive = false;
    let mut force = false;
    let mut paths = Vec::new();
    for argument in context.args {
        if let Some(flags) = argument.strip_prefix('-') {
            if flags.is_empty() {
                paths.push(argument);
            } else if flags.chars().all(|flag| matches!(flag, 'r' | 'f')) {
                recursive |= flags.contains('r');
                force |= flags.contains('f');
            } else {
                return usage("rm", "usage: rm [-rf] path ...");
            }
        } else {
            paths.push(argument);
        }
    }
    if paths.is_empty() {
        return usage("rm", "usage: rm [-rf] path ...");
    }
    for path in paths {
        if let Err(error) = context.fs.remove(path, recursive, force) {
            return fs_failure("rm", &error);
        }
    }
    CommandOutput::success("")
}

pub(super) fn cp(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut recursive = false;
    let mut paths = Vec::new();
    for argument in context.args {
        if let Some(flags) = argument.strip_prefix('-') {
            if flags.is_empty() {
                paths.push(argument.as_str());
            } else if flags.chars().all(|flag| matches!(flag, 'r' | 'R')) {
                recursive = true;
            } else {
                return usage("cp", "usage: cp [-r] source destination");
            }
        } else {
            paths.push(argument.as_str());
        }
    }
    if paths.len() != 2 {
        return usage("cp", "usage: cp [-r] source destination");
    }
    context.fs.copy(paths[0], paths[1], recursive).map_or_else(
        |error| fs_failure("cp", &error),
        |()| CommandOutput::success(""),
    )
}

pub(super) fn mv(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 2 {
        return usage("mv", "usage: mv source destination");
    }
    context
        .fs
        .move_path(&context.args[0], &context.args[1])
        .map_or_else(
            |error| fs_failure("mv", &error),
            |()| CommandOutput::success(""),
        )
}
