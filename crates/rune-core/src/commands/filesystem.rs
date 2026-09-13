use crate::{fs_failure, usage, CommandContext, CommandOutput};

const FIND_ENTRY_LIMIT: usize = 10_000;

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
        Ok(()) => CommandOutput::success(""),
        Err(error) => fs_failure("cd", &error),
    }
}

pub(super) fn ls(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut show_hidden = false;
    let mut paths = Vec::new();
    for argument in context.args {
        if let Some(flags) = argument.strip_prefix('-') {
            if flags.is_empty() {
                paths.push(argument.as_str());
            } else if flags.chars().all(|flag| flag == 'a') {
                show_hidden = true;
            } else {
                return usage("ls", "usage: ls [-a] [path ...]");
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
                stdout.push_str(&info.name);
                stdout.push('\n');
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
            stdout.push_str(&entry.name);
            if entry.is_directory {
                stdout.push('/');
            }
            stdout.push('\n');
        }
    }
    CommandOutput::success(stdout)
}

pub(super) fn cat(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return CommandOutput::success(context.stdin);
    }
    let mut stdout = String::new();
    for path in context.args {
        match context.fs.read(path) {
            Ok(bytes) => stdout.push_str(&String::from_utf8_lossy(&bytes)),
            Err(error) => return fs_failure("cat", &error),
        }
    }
    CommandOutput::success(stdout)
}

pub(super) fn find(context: &mut CommandContext<'_>) -> CommandOutput {
    let (path, pattern, max_depth) = match parse_find_args(context.args) {
        Ok(parsed) => parsed,
        Err(output) => return output,
    };
    let mut stdout = String::new();
    let mut visited = 0;
    if let Err(output) = visit_find(
        context,
        &path,
        pattern.as_deref(),
        max_depth,
        0,
        &mut visited,
        &mut stdout,
    ) {
        return output;
    }
    CommandOutput::success(stdout)
}

fn parse_find_args(
    args: &[String],
) -> Result<(String, Option<String>, Option<usize>), CommandOutput> {
    let mut path = ".".to_string();
    let mut pattern = None;
    let mut max_depth = None;
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
                        "usage: find [path] [-name PATTERN] [-maxdepth N]",
                    ));
                }
                if index + 1 != args.len() {
                    return Err(usage(
                        "find",
                        "usage: find [path] [-name PATTERN] [-maxdepth N]",
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
                pattern = Some(value.clone());
                index += 1;
            }
            "-maxdepth" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(usage("find", "-maxdepth requires a non-negative number"));
                };
                max_depth = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| usage("find", "-maxdepth requires a non-negative number"))?,
                );
                index += 1;
            }
            _ => {
                return Err(usage(
                    "find",
                    "usage: find [path] [-name PATTERN] [-maxdepth N]",
                ))
            }
        }
    }
    Ok((path, pattern, max_depth))
}

fn visit_find(
    context: &mut CommandContext<'_>,
    path: &str,
    pattern: Option<&str>,
    max_depth: Option<usize>,
    depth: usize,
    visited: &mut usize,
    stdout: &mut String,
) -> Result<(), CommandOutput> {
    if *visited >= FIND_ENTRY_LIMIT {
        return Err(CommandOutput::failure(
            1,
            format!("find: traversal exceeded {FIND_ENTRY_LIMIT} entries\n"),
        ));
    }
    let info = context
        .fs
        .metadata(path)
        .map_err(|error| fs_failure("find", &error))?;
    *visited += 1;
    if pattern.map_or(true, |value| wildcard_match(value, path_basename(path))) {
        stdout.push_str(path);
        stdout.push('\n');
    }
    if !info.is_directory || info.is_symlink || max_depth.is_some_and(|limit| depth >= limit) {
        return Ok(());
    }
    let entries = context
        .fs
        .list(Some(path))
        .map_err(|error| fs_failure("find", &error))?;
    for entry in entries {
        let child = append_child_path(path, &entry.name);
        visit_find(
            context,
            &child,
            pattern,
            max_depth,
            depth + 1,
            visited,
            stdout,
        )?;
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
