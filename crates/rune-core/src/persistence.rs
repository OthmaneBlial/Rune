use std::collections::BTreeMap;

use rune_fs::{FsError, VirtualFileSystem};

const STATE_DIRECTORY: &str = "~/.rune";
const STATE_PATH: &str = "~/.rune/session.state";
const STATE_HEADER: &str = "RUNE_SESSION_STATE_V1";
const HISTORY_LIMIT: usize = 1_000;
pub(super) const PROFILE_PATH: &str = "~/.rune_profile";
const PROFILE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Default)]
pub(super) struct SessionState {
    pub current_directory: Option<String>,
    pub history: Vec<String>,
    pub bookmarks: BTreeMap<String, String>,
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
    bookmarks: &BTreeMap<String, String>,
) -> Result<(), FsError> {
    match filesystem.make_directory(STATE_DIRECTORY, false) {
        Ok(()) | Err(FsError::AlreadyExists(_)) => {}
        Err(error) => return Err(error),
    }
    let content = serialize(current_directory, history, bookmarks);
    filesystem.write(STATE_PATH, content.as_bytes(), false)
}

pub(super) fn load_profile(filesystem: &dyn VirtualFileSystem) -> Result<Vec<String>, FsError> {
    let bytes = match filesystem.read(PROFILE_PATH) {
        Ok(bytes) => bytes,
        Err(FsError::NotFound(_)) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if bytes.len() > PROFILE_LIMIT {
        return Err(FsError::Io {
            operation: "read profile".to_string(),
            path: PROFILE_PATH.to_string(),
            message: format!("profile exceeds the {PROFILE_LIMIT}-byte limit"),
        });
    }
    let content = String::from_utf8(bytes).map_err(|_| FsError::Io {
        operation: "read profile".to_string(),
        path: PROFILE_PATH.to_string(),
        message: "profile is not valid UTF-8".to_string(),
    })?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

fn serialize(
    current_directory: &str,
    history: &[String],
    bookmarks: &BTreeMap<String, String>,
) -> String {
    let mut content = format!("{STATE_HEADER}\ncwd={}\n", escape(current_directory));
    for command in history.iter().rev().take(HISTORY_LIMIT).rev() {
        content.push_str("history=");
        content.push_str(&escape(command));
        content.push('\n');
    }
    for (name, path) in bookmarks {
        content.push_str("bookmark=");
        content.push_str(&escape(name));
        content.push('\t');
        content.push_str(&escape(path));
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
        let (key, raw_value) = line.split_once('=')?;
        match key {
            "cwd" if state.current_directory.is_none() => {
                state.current_directory = Some(unescape(raw_value)?);
            }
            "history" if state.history.len() < HISTORY_LIMIT => {
                state.history.push(unescape(raw_value)?);
            }
            "bookmark" => {
                let (raw_name, raw_path) = raw_value.split_once('\t')?;
                let name = unescape(raw_name)?;
                let path = unescape(raw_path)?;
                if !is_valid_bookmark_name(&name) || state.bookmarks.contains_key(&name) {
                    return None;
                }
                state.bookmarks.insert(name, path);
            }
            _ => return None,
        }
    }
    Some(state)
}

fn is_valid_bookmark_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
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
    use std::collections::BTreeMap;

    #[test]
    fn round_trips_state_without_treating_newlines_as_records() {
        let mut bookmarks = BTreeMap::new();
        bookmarks.insert("project".to_string(), "~/work".to_string());
        let content = serialize(
            "~/work",
            &["echo 100%".to_string(), "echo tab\tvalue".to_string()],
            &bookmarks,
        );
        let state = parse(&content).expect("valid state");
        assert_eq!(state.current_directory.as_deref(), Some("~/work"));
        assert_eq!(state.history, ["echo 100%", "echo tab\tvalue"]);
        assert_eq!(state.bookmarks["project"], "~/work");
    }

    #[test]
    fn rejects_unknown_or_malformed_records() {
        assert!(parse("RUNE_SESSION_STATE_V1\ncwd=~\nunknown=value\n").is_none());
        assert!(parse("RUNE_SESSION_STATE_V1\ncwd=%\n").is_none());
        assert!(
            parse("RUNE_SESSION_STATE_V1\nbookmark=project\t~\nbookmark=project\t~/work\n")
                .is_none()
        );
    }
}
