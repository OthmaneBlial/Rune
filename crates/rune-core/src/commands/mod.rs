use crate::CommandDefinition;

mod filesystem;
mod shell;
mod text;

pub(super) const DEFINITIONS: &[CommandDefinition] = &[
    CommandDefinition {
        name: "alias",
        summary: "define or print session-local command aliases",
        handler: shell::alias,
    },
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
        name: "export",
        summary: "set session environment variables",
        handler: shell::export,
    },
    CommandDefinition {
        name: "false",
        summary: "return a failure status",
        handler: shell::false_command,
    },
    CommandDefinition {
        name: "find",
        summary: "walk the bounded filesystem tree",
        handler: filesystem::find,
    },
    CommandDefinition {
        name: "grep",
        summary: "filter text lines by a pattern",
        handler: text::grep,
    },
    CommandDefinition {
        name: "head",
        summary: "write the first lines of input",
        handler: text::head,
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
        name: "sed",
        summary: "apply bounded literal substitutions to text",
        handler: text::sed,
    },
    CommandDefinition {
        name: "sort",
        summary: "sort input lines",
        handler: text::sort,
    },
    CommandDefinition {
        name: "tail",
        summary: "write the last lines of input",
        handler: text::tail,
    },
    CommandDefinition {
        name: "touch",
        summary: "create a file if it does not exist",
        handler: filesystem::touch,
    },
    CommandDefinition {
        name: "printenv",
        summary: "print selected environment variables",
        handler: shell::printenv,
    },
    CommandDefinition {
        name: "setenv",
        summary: "set one environment variable",
        handler: shell::setenv,
    },
    CommandDefinition {
        name: "true",
        summary: "return a success status",
        handler: shell::true_command,
    },
    CommandDefinition {
        name: "unalias",
        summary: "remove session-local command aliases",
        handler: shell::unalias,
    },
    CommandDefinition {
        name: "unset",
        summary: "remove session environment variables",
        handler: shell::unset,
    },
    CommandDefinition {
        name: "uniq",
        summary: "collapse adjacent duplicate lines",
        handler: text::uniq,
    },
    CommandDefinition {
        name: "wc",
        summary: "count lines, words, and bytes",
        handler: text::wc,
    },
];
