use crate::{fs_failure, usage, CommandContext, CommandOutput};
use rune_runtime::{RuntimeKind, RuntimeRequest, MAX_LUA_SOURCE_BYTES};

pub(super) fn lua(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(source_path) = context.args.first() else {
        return usage("lua", "usage: lua SCRIPT [ARG ...]");
    };
    match context.fs.metadata(source_path) {
        Ok(info) if info.size > MAX_LUA_SOURCE_BYTES as u64 => {
            return CommandOutput::failure(
                1,
                format!("lua: {source_path}: source exceeds {MAX_LUA_SOURCE_BYTES} bytes\n"),
            );
        }
        Ok(_) => {}
        Err(error) => return fs_failure("lua", &error),
    }
    let source = match context.fs.read(source_path) {
        Ok(source) => source,
        Err(error) => return fs_failure("lua", &error),
    };
    let request = RuntimeRequest::new(
        RuntimeKind::Lua,
        source_path,
        &source,
        &context.args[1..],
        context.env,
        context.stdin,
    )
    .with_cancellation(Some(context.cancellation));
    let execution = match context.lua_runtime.execute(&request) {
        Ok(execution) => execution,
        Err(error) => {
            return CommandOutput::failure(126, format!("lua: {source_path}: {error}\n"));
        }
    };
    CommandOutput {
        stdout: execution.stdout,
        stderr: execution.stderr,
        status: execution.status,
    }
}
