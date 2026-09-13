use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

pub(super) fn config(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(operation) = context.args.first().map(String::as_str) else {
        return show(context);
    };
    match operation {
        "get" if context.args.len() == 2 => get(context, &context.args[1]),
        "set" if context.args.len() == 3 => set(context, &context.args[1], &context.args[2]),
        "set" => usage(
            "config",
            "usage: config set history-limit|font-size|scrollback-limit|toolbar-visible|theme VALUE",
        ),
        "reset" if context.args.len() == 1 => reset(context),
        "get" => usage(
            "config",
            "usage: config get history-limit|font-size|scrollback-limit|toolbar-visible|theme",
        ),
        _ => usage("config", "usage: config [get KEY|set KEY VALUE|reset]"),
    }
}

fn show(context: &CommandContext<'_>) -> CommandOutput {
    let mut stdout = String::new();
    let _ = writeln!(stdout, "history-limit={}", context.config.history_limit());
    let _ = writeln!(stdout, "font-size={}", context.config.font_size());
    let _ = writeln!(
        stdout,
        "scrollback-limit={}",
        context.config.scrollback_limit()
    );
    let _ = writeln!(
        stdout,
        "toolbar-visible={}",
        context.config.toolbar_visible()
    );
    let _ = writeln!(stdout, "theme={}", context.config.theme().as_str());
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
        "scrollback-limit" => CommandOutput::success(format!(
            "scrollback-limit={}\n",
            context.config.scrollback_limit()
        )),
        "toolbar-visible" => CommandOutput::success(format!(
            "toolbar-visible={}\n",
            context.config.toolbar_visible()
        )),
        "theme" => CommandOutput::success(format!("theme={}\n", context.config.theme().as_str())),
        _ => usage(
            "config",
            "unknown key; available keys: history-limit, font-size, scrollback-limit, toolbar-visible, theme",
        ),
    }
}

fn set(context: &mut CommandContext<'_>, key: &str, value: &str) -> CommandOutput {
    let result = crate::config::update(context.fs, context.config, key, value);
    match result {
        Ok(()) => CommandOutput::success(""),
        Err(message) if message.starts_with("unknown key") => usage("config", &message),
        Err(message) => CommandOutput::failure(2, format!("config: {message}\n")),
    }
}

fn reset(context: &mut CommandContext<'_>) -> CommandOutput {
    if let Err(error) = crate::config::reset(context.fs, context.config) {
        return fs_failure("config", &error);
    }
    CommandOutput::success("")
}
