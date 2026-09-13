use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

pub(super) fn config(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(operation) = context.args.first().map(String::as_str) else {
        return show(context);
    };
    match operation {
        "get" if context.args.len() == 2 => get(context, &context.args[1]),
        "set" if context.args.len() == 3 => set(context, &context.args[1], &context.args[2]),
        "set" => usage("config", "usage: config set history-limit|font-size VALUE"),
        "reset" if context.args.len() == 1 => reset(context),
        "get" => usage("config", "usage: config get history-limit|font-size"),
        _ => usage(
            "config",
            "usage: config [get history-limit|font-size|set KEY VALUE|reset]",
        ),
    }
}

fn show(context: &CommandContext<'_>) -> CommandOutput {
    let mut stdout = String::new();
    let _ = writeln!(stdout, "history-limit={}", context.config.history_limit());
    let _ = writeln!(stdout, "font-size={}", context.config.font_size());
    CommandOutput::success(stdout)
}

fn get(context: &CommandContext<'_>, key: &str) -> CommandOutput {
    match key {
        "history-limit" => CommandOutput::success(format!(
            "history-limit={}\n",
            context.config.history_limit()
        )),
        "font-size" => {
            CommandOutput::success(format!("font-size={}\n", context.config.font_size()))
        }
        _ => usage(
            "config",
            "unknown key; available keys: history-limit, font-size",
        ),
    }
}

fn set(context: &mut CommandContext<'_>, key: &str, value: &str) -> CommandOutput {
    let result = match key {
        "history-limit" => crate::config::update_history_limit(context.fs, context.config, value),
        "font-size" => crate::config::update_font_size(context.fs, context.config, value),
        _ => {
            return usage(
                "config",
                "unknown key; available keys: history-limit, font-size",
            )
        }
    };
    match result {
        Ok(()) => CommandOutput::success(""),
        Err(message) => CommandOutput::failure(2, format!("config: {message}\n")),
    }
}

fn reset(context: &mut CommandContext<'_>) -> CommandOutput {
    let previous = context.config.clone();
    *context.config = crate::config::TerminalConfig::default();
    if let Err(error) = context.config.save(context.fs) {
        *context.config = previous;
        return fs_failure("config", &error);
    }
    CommandOutput::success("")
}
