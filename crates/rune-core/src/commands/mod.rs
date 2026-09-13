use crate::CommandDefinition;

mod filesystem;
mod shell;

pub(super) const DEFINITIONS: &[CommandDefinition] = &[
    CommandDefinition {
        name: "cat",
        summary: "write file contents to stdout",
        handler: filesystem::cat,
    },
    CommandDefinition {
        name: "cd",
        summary: "change the current directory",
        handler: filesystem::cd,
    },
    CommandDefinition {
        name: "clear",
        summary: "clear the terminal display",
        handler: shell::clear,
    },
    CommandDefinition {
        name: "cp",
        summary: "copy one file",
        handler: filesystem::cp,
    },
    CommandDefinition {
        name: "echo",
        summary: "write arguments to stdout",
        handler: shell::echo,
    },
    CommandDefinition {
        name: "env",
        summary: "print the session environment",
        handler: shell::env,
    },
    CommandDefinition {
        name: "help",
        summary: "list commands and their summaries",
        handler: shell::help,
    },
    CommandDefinition {
        name: "history",
        summary: "print command history",
        handler: shell::history,
    },
    CommandDefinition {
        name: "ls",
        summary: "list directory entries",
        handler: filesystem::ls,
    },
    CommandDefinition {
        name: "mkdir",
        summary: "create directories",
        handler: filesystem::mkdir,
    },
    CommandDefinition {
        name: "mv",
        summary: "move one file",
        handler: filesystem::mv,
    },
    CommandDefinition {
        name: "pwd",
        summary: "print the current directory",
        handler: shell::pwd,
    },
    CommandDefinition {
        name: "rm",
        summary: "remove files or directories",
        handler: filesystem::rm,
    },
    CommandDefinition {
        name: "touch",
        summary: "create a file if it does not exist",
        handler: filesystem::touch,
    },
];
