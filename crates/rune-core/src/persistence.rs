use rune_fs::{FsError, VirtualFileSystem};

const STATE_DIRECTORY: &str = "~/.rune";
const STATE_PATH: &str = "~/.rune/session.state";
const STATE_HEADER: &str = "RUNE_SESSION_STATE_V1";
const HISTORY_LIMIT: usize = 1_000;

#[derive(Debug, Default)]
pub(super) struct SessionState {
    pub current_directory: Option<String>,
    pub history: Vec<String>,
}

pub(super) fn load(filesystem: &dyn VirtualFileSystem) -> SessionState {
    let Ok(bytes) = filesystem.read(STATE_PATH) else {
        return SessionState::default();
    };
    let Ok(content) = String::from_utf8(bytes) else {
        return SessionState::default();
    };
    parse(&content).unwrap_or_default()
}

pub(super) fn save(
    filesystem: &mut dyn VirtualFileSystem,
    current_directory: &str,
    history: &[String],
) -> Result<(), FsError> {
    match filesystem.make_directory(STATE_DIRECTORY, false) {
        Ok(()) | Err(FsError::AlreadyExists(_)) => {}
        Err(error) => return Err(error),
    }
    let content = serialize(current_directory, history);
    filesystem.write(STATE_PATH, content.as_bytes(), false)
}

fn serialize(current_directory: &str, history: &[String]) -> String {
    let mut content = format!("{STATE_HEADER}\ncwd={}\n", escape(current_directory));
    for command in history.iter().rev().take(HISTORY_LIMIT).rev() {
        content.push_str("history=");
        content.push_str(&escape(command));
        content.push('\n');
    }
    content
}

fn parse(content: &str) -> Option<SessionState> {
    let mut lines = content.lines();
    if lines.next()? != STATE_HEADER {
        return None;
    }
    let mut state = SessionState::default();
    for line in lines {
        let (key, value) = line.split_once('=')?;
        let value = unescape(value)?;
        match key {
            "cwd" if state.current_directory.is_none() => state.current_directory = Some(value),
            "history" if state.history.len() < HISTORY_LIMIT => state.history.push(value),
            _ => return None,
        }
    }
    Some(state)
}

fn escape(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('\t', "%09")
}

fn unescape(value: &str) -> Option<String> {
    let mut output = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            output.push(character);
            continue;
        }
        let high = characters.next()?;
        let low = characters.next()?;
        let code = u8::from_str_radix(&format!("{high}{low}"), 16).ok()?;
        output.push(char::from(code));
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::{parse, serialize};

    #[test]
    fn round_trips_state_without_treating_newlines_as_records() {
        let content = serialize(
            "~/work",
            &["echo 100%".to_string(), "echo tab\tvalue".to_string()],
        );
        let state = parse(&content).expect("valid state");
        assert_eq!(state.current_directory.as_deref(), Some("~/work"));
        assert_eq!(state.history, ["echo 100%", "echo tab\tvalue"]);
    }

    #[test]
    fn rejects_unknown_or_malformed_records() {
        assert!(parse("RUNE_SESSION_STATE_V1\ncwd=~\nunknown=value\n").is_none());
        assert!(parse("RUNE_SESSION_STATE_V1\ncwd=%\n").is_none());
    }
}
