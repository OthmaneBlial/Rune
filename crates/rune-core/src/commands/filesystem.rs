use crate::{fs_failure, usage, CommandContext, CommandOutput};

pub(super) fn cd(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() > 1 {
        return usage("cd", "usage: cd [directory]");
    }
    let directory = context.args.first().map_or("~", String::as_str);
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
    if context.args.len() != 2 {
        return usage("cp", "usage: cp source destination");
    }
    context
        .fs
        .copy(&context.args[0], &context.args[1])
        .map_or_else(
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
