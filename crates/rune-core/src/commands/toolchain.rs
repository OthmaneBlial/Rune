use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};
use rune_runtime::{ToolchainKind, ToolchainRequest, MAX_TOOLCHAIN_SOURCE_BYTES};

pub(super) fn c(context: &mut CommandContext<'_>) -> CommandOutput {
    run(context, ToolchainKind::C, "cc")
}

pub(super) fn cpp(context: &mut CommandContext<'_>) -> CommandOutput {
    run(context, ToolchainKind::Cpp, "c++")
}

pub(super) fn tex(context: &mut CommandContext<'_>) -> CommandOutput {
    run(context, ToolchainKind::Tex, "tex")
}

fn run(context: &mut CommandContext<'_>, kind: ToolchainKind, command: &str) -> CommandOutput {
    let Some(source_path) = context.args.first() else {
        return usage(command, &format!("usage: {command} SOURCE [ARG ...]"));
    };
    if matches!(source_path.as_str(), "--help" | "-h") {
        return CommandOutput::success(format!(
            "usage: {command} SOURCE [ARG ...]\nRuns through an explicit Rune {kind} toolchain provider; no host compiler is started.\n"
        ));
    }
    if source_path == "--version" {
        return CommandOutput::success(format!(
            "{command}: Rune {kind} toolchain interface (provider required)\n"
        ));
    }
    if source_path.starts_with('-') {
        return usage(command, &format!("usage: {command} SOURCE [ARG ...]"));
    }
    match context.fs.metadata(source_path) {
        Ok(info) if info.size > MAX_TOOLCHAIN_SOURCE_BYTES as u64 => {
            return CommandOutput::failure(
                1,
                format!(
                    "{command}: {source_path}: source exceeds {MAX_TOOLCHAIN_SOURCE_BYTES} bytes\n"
                ),
            );
        }
        Ok(_) => {}
        Err(error) => return fs_failure(command, &error),
    }
    let source = match context.fs.read(source_path) {
        Ok(source) => source,
        Err(error) => return fs_failure(command, &error),
    };
    let request = ToolchainRequest::new(
        kind,
        source_path,
        &source,
        &context.args[1..],
        context.env,
        context.stdin,
    )
    .with_cancellation(Some(context.cancellation));
    if let Err(error) = request.validate() {
        return CommandOutput::failure(2, format!("{command}: {error}\n"));
    }
    let execution = match context.toolchains.provider(kind).execute(&request) {
        Ok(execution) => execution,
        Err(error) => return CommandOutput::failure(126, format!("{command}: {error}\n")),
    };
    if let Err(error) = execution.validate() {
        return CommandOutput::failure(
            1,
            format!("{command}: provider output rejected: {error}\n"),
        );
    }
    let mut output = CommandOutput {
        stdout: execution.stdout,
        stderr: execution.stderr,
        status: execution.status,
    };
    if output.status == 0 {
        for artifact in execution.artifacts {
            if let Err(error) = context.fs.write(&artifact.path, &artifact.bytes, false) {
                output.status = 1;
                let _ = writeln!(
                    output.stderr,
                    "{command}: cannot materialize {}: {error}",
                    artifact.path
                );
                break;
            }
        }
    }
    output
}
