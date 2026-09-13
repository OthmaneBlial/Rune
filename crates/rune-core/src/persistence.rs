use std::collections::BTreeMap;

use rune_fs::{FsError, VirtualFileSystem};

use super::{MAX_BOOKMARKS, MAX_BOOKMARK_BYTES};

const STATE_DIRECTORY: &str = "~/.rune";
const STATE_PATH: &str = "~/.rune/session.state";
const SESSION_DIRECTORY: &str = "~/.rune/sessions";
const STATE_HEADER: &str = "RUNE_SESSION_STATE_V1";
pub(super) const MAX_HISTORY_ENTRIES: usize = 10_000;
pub(super) const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;
const MAX_SESSION_STATE_BYTES: usize = MAX_HISTORY_BYTES + MAX_BOOKMARK_BYTES + 1_024;
pub(super) const PROFILE_PATH: &str = "~/.rune_profile";
const PROFILE_LIMIT: usize = 64 * 1024;
pub(super) const MAX_SESSION_ID_CHARS: usize = 64;

#[derive(Debug, Default)]
pub(super) struct SessionState {
    pub current_directory: Option<String>,
    pub history: Vec<String>,
    pub bookmarks: BTreeMap<String, String>,
}

pub(super) fn load(filesystem: &dyn VirtualFileSystem, session_id: Option<&str>) -> SessionState {
    let (_, state_path) = state_paths(session_id);
    let Ok(bytes) = filesystem.read(&state_path) else {
        return SessionState::default();
    };
    if bytes.len() > MAX_SESSION_STATE_BYTES {
        return SessionState::default();
    }
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
    session_id: Option<&str>,
) -> Result<(), FsError> {
    let (state_directory, state_path) = state_paths(session_id);
    match filesystem.make_directory(&state_directory, true) {
        Ok(()) | Err(FsError::AlreadyExists(_)) => {}
        Err(error) => return Err(error),
    }
    let content = serialize(current_directory, history, bookmarks);
    if content.len() > MAX_SESSION_STATE_BYTES {
        return Err(FsError::Io {
            operation: "serialize session".to_string(),
            path: state_path,
            message: format!("session state exceeds the {MAX_SESSION_STATE_BYTES}-byte limit"),
        });
    }
    filesystem.write(&state_path, content.as_bytes(), false)
}

pub(super) fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.chars().count() <= MAX_SESSION_ID_CHARS
        && session_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

fn state_paths(session_id: Option<&str>) -> (String, String) {
    match session_id {
        Some(session_id) => (
            format!("{SESSION_DIRECTORY}/{session_id}"),
            format!("{SESSION_DIRECTORY}/{session_id}/session.state"),
        ),
        None => (STATE_DIRECTORY.to_string(), STATE_PATH.to_string()),
    }
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
    let mut persisted_history = Vec::new();
    let mut history_bytes = 0_usize;
    for command in history.iter().rev().take(MAX_HISTORY_ENTRIES) {
        let escaped = escape(command);
        let record_bytes = "history=".len() + escaped.len() + 1;
        if history_bytes.saturating_add(record_bytes) > MAX_HISTORY_BYTES {
            break;
        }
        history_bytes += record_bytes;
        persisted_history.push(escaped);
    }
    for escaped in persisted_history.iter().rev() {
        content.push_str("history=");
        content.push_str(escaped);
        content.push('\n');
    }
    let mut bookmark_bytes = 0_usize;
    for (index, (name, path)) in bookmarks.iter().enumerate() {
        if index >= MAX_BOOKMARKS {
            break;
        }
        let escaped_name = escape(name);
        let escaped_path = escape(path);
        let record_bytes = "bookmark=".len() + escaped_name.len() + 1 + escaped_path.len() + 1;
        if bookmark_bytes.saturating_add(record_bytes) > MAX_BOOKMARK_BYTES {
            break;
        }
        bookmark_bytes += record_bytes;
        content.push_str("bookmark=");
        content.push_str(&escaped_name);
        content.push('\t');
        content.push_str(&escaped_path);
        content.push('\n');
    }
    content
}

fn parse(content: &str) -> Option<SessionState> {
    if content.len() > MAX_SESSION_STATE_BYTES {
        return None;
    }
    let mut lines = content.lines();
    if lines.next()? != STATE_HEADER {
        return None;
    }
    let mut state = SessionState::default();
    let mut history_bytes = 0_usize;
    let mut bookmark_bytes = 0_usize;
    for line in lines {
        let (key, raw_value) = line.split_once('=')?;
        match key {
            "cwd" if state.current_directory.is_none() => {
                state.current_directory = Some(unescape(raw_value)?);
            }
            "history" if state.history.len() < MAX_HISTORY_ENTRIES => {
                let record_bytes = "history=".len() + raw_value.len() + 1;
                if history_bytes.saturating_add(record_bytes) > MAX_HISTORY_BYTES {
                    return None;
                }
                history_bytes += record_bytes;
                state.history.push(unescape(raw_value)?);
            }
            "bookmark" => {
                if state.bookmarks.len() >= MAX_BOOKMARKS {
                    return None;
                }
                let record_bytes = "bookmark=".len() + raw_value.len() + 1;
                if bookmark_bytes.saturating_add(record_bytes) > MAX_BOOKMARK_BYTES {
                    return None;
                }
                bookmark_bytes += record_bytes;
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

pub(super) fn apply_history_limit(history: &mut Vec<String>, max_entries: usize) {
    let max_entries = max_entries.min(MAX_HISTORY_ENTRIES);
    if history.len() > max_entries {
        let excess = history.len() - max_entries;
        history.drain(0..excess);
    }

    let mut retained_bytes = 0_usize;
    let mut keep_from = history.len();
    for (index, command) in history.iter().enumerate().rev() {
        let record_bytes = "history=".len() + escape(command).len() + 1;
        if retained_bytes.saturating_add(record_bytes) > MAX_HISTORY_BYTES {
            break;
        }
        retained_bytes += record_bytes;
        keep_from = index;
    }
    if keep_from > 0 {
        history.drain(0..keep_from);
    }
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
    use super::{parse, serialize, MAX_BOOKMARK_BYTES, MAX_HISTORY_BYTES};
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

    #[test]
    fn bounds_serialized_history_by_bytes() {
        let commands = (0..2_000)
            .map(|index| format!("echo {}", "x".repeat(4_096 + index % 8)))
            .collect::<Vec<_>>();
        let content = serialize("~", &commands, &BTreeMap::new());
        assert!(content.len() <= MAX_HISTORY_BYTES + 64);
        let state = parse(&content).expect("bounded state should remain valid");
        assert!(state.history.len() < commands.len());
        assert_eq!(state.history.last(), commands.last());
    }

    #[test]
    fn bounds_serialized_bookmarks_by_count_and_bytes() {
        let bookmarks = (0..512)
            .map(|index| {
                (
                    format!("mark{index:03}"),
                    format!("~/{}", "x".repeat(1_024 + index % 8)),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let content = serialize("~", &[], &bookmarks);
        assert!(content.len() <= MAX_HISTORY_BYTES + MAX_BOOKMARK_BYTES + 64);
        let state = parse(&content).expect("bounded state should remain valid");
        assert!(state.bookmarks.len() < bookmarks.len());
        assert!(state.bookmarks.contains_key("mark000"));
    }
}
