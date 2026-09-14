use crate::CommandDefinition;

mod ar;
mod archive;
mod awk;
mod bookmarks;
mod compression;
mod config;
mod filesystem;
mod javascript;
mod lua;
mod network;
mod open;
mod package;
mod python;
pub(super) mod shell;
mod test;
mod text;
mod toolchain;
mod utilities;
mod wasm;
mod xargs;

pub(crate) use xargs::{command_line as xargs_command_line, parse_plan as parse_xargs_plan};

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
        name: "apropos",
        summary: "search bounded Rune command descriptions",
        handler: shell::apropos,
    },
    CommandDefinition {
        name: "ar",
        summary: "create, list, or extract bounded ar archives",
        handler: ar::ar,
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
        name: "d",
        summary: "short alias for deleting saved directory bookmarks",
        handler: bookmarks::deletemark,
    },
    CommandDefinition {
        name: "d",
        summary: "short alias for deleting saved directory bookmarks",
        handler: bookmarks::deletemark,
    },
    CommandDefinition {
        name: "basename",
        summary: "print the final component of a path",
        handler: utilities::basename,
    },
    CommandDefinition {
        name: "break",
        summary: "leave the current bounded script loop",
        handler: shell::break_command,
    },
    CommandDefinition {
        name: "return",
        summary: "return a bounded status from a script function",
        handler: shell::return_command,
    },
    CommandDefinition {
        name: "local",
        summary: "set bounded function-local variables",
        handler: shell::local_command,
    },
    CommandDefinition {
        name: "shift",
        summary: "shift bounded script positional arguments",
        handler: shell::shift_command,
    },
    CommandDefinition {
        name: "set",
        summary: "set bounded script positional arguments",
        handler: shell::set_command,
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
        name: "c++",
        summary: "compile bounded C++ through an explicit toolchain provider",
        handler: toolchain::cpp,
    },
    CommandDefinition {
        name: "cc",
        summary: "compile bounded C through an explicit toolchain provider",
        handler: toolchain::c,
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
        name: "command",
        summary: "discover bounded Rune and package commands",
        handler: shell::command,
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
        name: "dash",
        summary: "execute a bounded Rust-parsed script with dash -c",
        handler: shell::dash,
    },
    CommandDefinition {
        name: "config",
        summary: "inspect or change Rust-owned session settings",
        handler: config::config,
    },
    CommandDefinition {
        name: "continue",
        summary: "skip the current bounded script loop iteration",
        handler: shell::continue_command,
    },
    CommandDefinition {
        name: "curl",
        summary: "make a bounded HTTP request through the host network grant",
        handler: network::curl,
    },
    CommandDefinition {
        name: "clang",
        summary: "compile bounded C through an explicit toolchain provider",
        handler: toolchain::c,
    },
    CommandDefinition {
        name: "clang++",
        summary: "compile bounded C++ through an explicit toolchain provider",
        handler: toolchain::cpp,
    },
    CommandDefinition {
        name: "nslookup",
        summary: "resolve a bounded DNS name through host DNS-over-HTTPS",
        handler: network::nslookup,
    },
    CommandDefinition {
        name: "whois",
        summary: "fetch a bounded domain record through host HTTPS RDAP",
        handler: network::whois,
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
        name: "exit",
        summary: "request that the host close this terminal session",
        handler: shell::exit,
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
        name: "g",
        summary: "short alias for jumping to a saved directory bookmark",
        handler: bookmarks::jump,
    },
    CommandDefinition {
        name: "g",
        summary: "short alias for jumping to a saved directory bookmark",
        handler: bookmarks::jump,
    },
    CommandDefinition {
        name: "file",
        summary: "identify bounded VFS files without host libmagic",
        handler: utilities::file,
    },
    CommandDefinition {
        name: "find",
        summary: "walk the bounded filesystem tree",
        handler: filesystem::find,
    },
    CommandDefinition {
        name: "grep",
        summary: "filter text lines by a bounded regular expression",
        handler: text::grep,
    },
    CommandDefinition {
        name: "egrep",
        summary: "filter text lines using extended regular-expression syntax",
        handler: text::egrep,
    },
    CommandDefinition {
        name: "fgrep",
        summary: "filter text lines by a literal pattern",
        handler: text::fgrep,
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
        name: "l",
        summary: "short alias for listing saved directory bookmarks",
        handler: bookmarks::showmarks,
    },
    CommandDefinition {
        name: "l",
        summary: "short alias for listing saved directory bookmarks",
        handler: bookmarks::showmarks,
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
        summary: "run bounded Python code or a sandbox script",
        handler: python::python,
    },
    CommandDefinition {
        name: "python3",
        summary: "run bounded Python code or a sandbox script",
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
        name: "mktemp",
        summary: "create a unique bounded temporary file or directory",
        handler: utilities::mktemp,
    },
    CommandDefinition {
        name: "mv",
        summary: "move one file or directory",
        handler: filesystem::mv,
    },
    CommandDefinition {
        name: "newWindow",
        summary: "request that the host open an independent terminal window",
        handler: shell::new_window,
    },
    CommandDefinition {
        name: "new-window",
        summary: "request that the host open an independent terminal window",
        handler: shell::new_window,
    },
    CommandDefinition {
        name: "open",
        summary: "open a confined file or approved external URL through the host",
        handler: open::open,
    },
    CommandDefinition {
        name: "openurl",
        summary: "open an approved external URL through the host",
        handler: open::openurl,
    },
    CommandDefinition {
        name: "pwd",
        summary: "print the current directory",
        handler: shell::pwd,
    },
    CommandDefinition {
        name: "p",
        summary: "short alias for listing saved directory bookmarks",
        handler: bookmarks::showmarks,
    },
    CommandDefinition {
        name: "p",
        summary: "short alias for listing saved directory bookmarks",
        handler: bookmarks::showmarks,
    },
    CommandDefinition {
        name: "readlink",
        summary: "read a symbolic link target",
        handler: filesystem::readlink,
    },
    CommandDefinition {
        name: "r",
        summary: "short alias for renaming a directory bookmark",
        handler: bookmarks::renamemark,
    },
    CommandDefinition {
        name: "r",
        summary: "short alias for renaming a directory bookmark",
        handler: bookmarks::renamemark,
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
        name: "pickFolder",
        summary: "ask the host to choose a confined external folder",
        handler: shell::pick_folder,
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
        name: "s",
        summary: "short alias for saving the current directory bookmark",
        handler: bookmarks::bookmark,
    },
    CommandDefinition {
        name: "s",
        summary: "short alias for saving the current directory bookmark",
        handler: bookmarks::bookmark,
    },
    CommandDefinition {
        name: "sh",
        summary: "execute a bounded Rust-parsed script with sh -c",
        handler: shell::sh,
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
        name: "tex",
        summary: "render bounded TeX through an explicit toolchain provider",
        handler: toolchain::tex,
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
        name: "tree",
        summary: "render a bounded filesystem tree",
        handler: filesystem::tree,
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
        name: "test",
        summary: "evaluate bounded file, string, and integer predicates",
        handler: test::test_command,
    },
    CommandDefinition {
        name: "[",
        summary: "evaluate a bounded predicate with a closing bracket",
        handler: test::bracket_command,
    },
    CommandDefinition {
        name: "type",
        summary: "describe aliases, built-ins, and installed commands",
        handler: shell::type_command,
    },
    CommandDefinition {
        name: "unalias",
        summary: "remove session-local command aliases",
        handler: shell::unalias,
    },
    CommandDefinition {
        name: "unzip",
        summary: "list or extract bounded ZIP archives",
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
        name: "xargs",
        summary: "run bounded command batches from standard input",
        handler: xargs::xargs,
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
        summary: "create bounded ZIP archives",
        handler: archive::zip,
    },
];
