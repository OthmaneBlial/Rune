use crate::CommandDefinition;

mod archive;
mod awk;
mod bookmarks;
mod compression;
mod config;
mod filesystem;
mod javascript;
mod lua;
mod network;
mod package;
mod python;
pub(super) mod shell;
mod text;
mod utilities;
mod wasm;

pub(super) const DEFINITIONS: &[CommandDefinition] = &[
    CommandDefinition {
        name: ".",
        summary: "execute a bounded Rune script from a sandbox file",
        handler: shell::source,
    },
    CommandDefinition {
        name: "base64",
        summary: "encode or decode bounded base64 input",
        handler: utilities::base64,
    },
    CommandDefinition {
        name: "bc",
        summary: "evaluate bounded integer calculator expressions",
        handler: utilities::bc,
    },
    CommandDefinition {
        name: "alias",
        summary: "define or print session-local command aliases",
        handler: shell::alias,
    },
    CommandDefinition {
        name: "awk",
        summary: "process bounded text fields with a Rust-owned awk subset",
        handler: awk::awk,
    },
    CommandDefinition {
        name: "bookmark",
        summary: "save the current directory under a session-local name",
        handler: bookmarks::bookmark,
    },
    CommandDefinition {
        name: "basename",
        summary: "print the final component of a path",
        handler: utilities::basename,
    },
    CommandDefinition {
        name: "realpath",
        summary: "print the confined canonical path of an existing file",
        handler: utilities::realpath,
    },
    CommandDefinition {
        name: "cat",
        summary: "write file contents to stdout",
        handler: filesystem::cat,
    },
    CommandDefinition {
        name: "cksum",
        summary: "print a bounded POSIX CRC checksum",
        handler: utilities::cksum,
    },
    CommandDefinition {
        name: "date",
        summary: "print a bounded current date/time view",
        handler: utilities::date,
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
        name: "compress",
        summary: "compress bounded VFS files with LZW",
        handler: compression::compress,
    },
    CommandDefinition {
        name: "cp",
        summary: "copy files or bounded directory trees",
        handler: filesystem::cp,
    },
    CommandDefinition {
        name: "cut",
        summary: "select bounded fields or characters from text",
        handler: text::cut,
    },
    CommandDefinition {
        name: "config",
        summary: "inspect or change Rust-owned session settings",
        handler: config::config,
    },
    CommandDefinition {
        name: "curl",
        summary: "make a bounded HTTP request through the host network grant",
        handler: network::curl,
    },
    CommandDefinition {
        name: "deletemark",
        summary: "remove one or more saved directory names",
        handler: bookmarks::deletemark,
    },
    CommandDefinition {
        name: "dirname",
        summary: "print the parent component of a path",
        handler: utilities::dirname,
    },
    CommandDefinition {
        name: "diff",
        summary: "compare two bounded UTF-8 files",
        handler: text::diff,
    },
    CommandDefinition {
        name: "du",
        summary: "summarize bounded virtual filesystem bytes",
        handler: utilities::du,
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
        name: "expr",
        summary: "evaluate bounded integer and text expressions",
        handler: utilities::expr,
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
        name: "gzip",
        summary: "compress bounded VFS files with gzip",
        handler: compression::gzip,
    },
    CommandDefinition {
        name: "head",
        summary: "write the first lines of input",
        handler: text::head,
    },
    CommandDefinition {
        name: "gunzip",
        summary: "decompress bounded gzip files in the VFS",
        handler: compression::gunzip,
    },
    CommandDefinition {
        name: "help",
        summary: "list commands and their summaries",
        handler: shell::help,
    },
    CommandDefinition {
        name: "history",
        summary: "print or search command history",
        handler: shell::history,
    },
    CommandDefinition {
        name: "jump",
        summary: "change directory to a saved bookmark",
        handler: bookmarks::jump,
    },
    CommandDefinition {
        name: "jsc",
        summary: "run a bounded sandbox JavaScript file",
        handler: javascript::jsc,
    },
    CommandDefinition {
        name: "ls",
        summary: "list directory entries",
        handler: filesystem::ls,
    },
    CommandDefinition {
        name: "ln",
        summary: "create a bounded relative symbolic link",
        handler: filesystem::ln,
    },
    CommandDefinition {
        name: "lua",
        summary: "run a bounded sandbox Lua 5.4 script",
        handler: lua::lua,
    },
    CommandDefinition {
        name: "python",
        summary: "run a bounded sandbox Python script",
        handler: python::python,
    },
    CommandDefinition {
        name: "python3",
        summary: "run a bounded sandbox Python script",
        handler: python::python,
    },
    CommandDefinition {
        name: "mkdir",
        summary: "create directories",
        handler: filesystem::mkdir,
    },
    CommandDefinition {
        name: "md5",
        summary: "print a bounded MD5 digest for compatibility",
        handler: utilities::md5,
    },
    CommandDefinition {
        name: "mv",
        summary: "move one file or directory",
        handler: filesystem::mv,
    },
    CommandDefinition {
        name: "pwd",
        summary: "print the current directory",
        handler: shell::pwd,
    },
    CommandDefinition {
        name: "readlink",
        summary: "read a symbolic link target",
        handler: filesystem::readlink,
    },
    CommandDefinition {
        name: "pkg",
        summary: "inspect, verify, install, update, search, list, or remove packages",
        handler: package::pkg,
    },
    CommandDefinition {
        name: "pbcopy",
        summary: "copy bounded stdin text through the host clipboard capability",
        handler: shell::pbcopy,
    },
    CommandDefinition {
        name: "pbpaste",
        summary: "paste bounded text from the host clipboard capability",
        handler: shell::pbpaste,
    },
    CommandDefinition {
        name: "rm",
        summary: "remove files or directories",
        handler: filesystem::rm,
    },
    CommandDefinition {
        name: "rmdir",
        summary: "remove empty directories",
        handler: utilities::rmdir,
    },
    CommandDefinition {
        name: "renamemark",
        summary: "rename a saved directory bookmark",
        handler: bookmarks::renamemark,
    },
    CommandDefinition {
        name: "sed",
        summary: "apply bounded literal substitutions to text",
        handler: text::sed,
    },
    CommandDefinition {
        name: "sha256",
        summary: "print a SHA-256 digest for bounded input",
        handler: utilities::sha256,
    },
    CommandDefinition {
        name: "sort",
        summary: "sort input lines",
        handler: text::sort,
    },
    CommandDefinition {
        name: "sum",
        summary: "print a bounded BSD or System V checksum",
        handler: utilities::sum,
    },
    CommandDefinition {
        name: "sleep",
        summary: "wait for a bounded duration with cooperative cancellation",
        handler: shell::sleep,
    },
    CommandDefinition {
        name: "source",
        summary: "execute a bounded Rune script from a sandbox file",
        handler: shell::source,
    },
    CommandDefinition {
        name: "stat",
        summary: "show bounded virtual filesystem metadata",
        handler: utilities::stat,
    },
    CommandDefinition {
        name: "tail",
        summary: "write the last lines of input",
        handler: text::tail,
    },
    CommandDefinition {
        name: "tar",
        summary: "create, list, or extract bounded USTAR archives",
        handler: archive::tar,
    },
    CommandDefinition {
        name: "tee",
        summary: "copy stdin to files and stdout",
        handler: utilities::tee,
    },
    CommandDefinition {
        name: "touch",
        summary: "create a file if it does not exist",
        handler: filesystem::touch,
    },
    CommandDefinition {
        name: "tr",
        summary: "translate or delete input characters",
        handler: utilities::tr,
    },
    CommandDefinition {
        name: "printenv",
        summary: "print selected environment variables",
        handler: shell::printenv,
    },
    CommandDefinition {
        name: "printf",
        summary: "format bounded text without an implicit newline",
        handler: shell::printf,
    },
    CommandDefinition {
        name: "setenv",
        summary: "set one environment variable",
        handler: shell::setenv,
    },
    CommandDefinition {
        name: "showmarks",
        summary: "list saved directory bookmarks",
        handler: bookmarks::showmarks,
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
        name: "unzip",
        summary: "extract bounded uncompressed ZIP archives",
        handler: archive::unzip,
    },
    CommandDefinition {
        name: "uncompress",
        summary: "decompress bounded .Z files with LZW",
        handler: compression::uncompress,
    },
    CommandDefinition {
        name: "unlink",
        summary: "remove one regular file",
        handler: utilities::unlink,
    },
    CommandDefinition {
        name: "unset",
        summary: "remove session environment variables",
        handler: shell::unset,
    },
    CommandDefinition {
        name: "unsetenv",
        summary: "remove session environment variables",
        handler: shell::unsetenv,
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
    CommandDefinition {
        name: "wasm",
        summary: "run a bounded WASI preview1 module",
        handler: wasm::wasm,
    },
    CommandDefinition {
        name: "uname",
        summary: "report the portable Rune identity",
        handler: shell::uname,
    },
    CommandDefinition {
        name: "which",
        summary: "describe aliases, built-ins, and installed package commands",
        handler: shell::which,
    },
    CommandDefinition {
        name: "whoami",
        summary: "report the stable virtual session identity",
        handler: shell::whoami,
    },
    CommandDefinition {
        name: "xxd",
        summary: "render bounded hexadecimal input",
        handler: utilities::xxd,
    },
    CommandDefinition {
        name: "zip",
        summary: "create bounded uncompressed ZIP archives",
        handler: archive::zip,
    },
];
