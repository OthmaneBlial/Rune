//! Bounded in-memory diagnostics for development and support tooling.
//!
//! Diagnostic records are deliberately not persisted. Callers should record
//! only safe metadata such as statuses, byte counts, and validated capability
//! names; command text, file contents, environment values, and full paths do
//! not belong in this buffer.

use std::collections::VecDeque;
use std::fmt::Write as _;

pub(crate) const MAX_DIAGNOSTIC_ENTRIES: usize = 128;
pub(crate) const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 512;
pub(crate) const MAX_DIAGNOSTIC_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub(crate) enum DiagnosticLevel {
    Info,
    Warn,
    Error,
}

impl DiagnosticLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// A bounded FIFO of safe, non-persistent diagnostic records.
#[derive(Debug, Default)]
pub(crate) struct DiagnosticLog {
    records: VecDeque<String>,
    next_sequence: u64,
}

impl DiagnosticLog {
    pub(crate) fn record(
        &mut self,
        level: DiagnosticLevel,
        component: &'static str,
        message: impl AsRef<str>,
    ) {
        let message = sanitize_message(message.as_ref());
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let record = format!("{sequence} {} {component}: {message}", level.as_str());
        if self.records.len() == MAX_DIAGNOSTIC_ENTRIES {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    pub(crate) fn snapshot(&self) -> String {
        let mut output = String::new();
        for record in &self.records {
            if output.len() + record.len() + 1 > MAX_DIAGNOSTIC_OUTPUT_BYTES {
                break;
            }
            let _ = writeln!(output, "{record}");
        }
        output
    }

    pub(crate) fn clear(&mut self) {
        self.records.clear();
    }
}

fn sanitize_message(message: &str) -> String {
    let mut sanitized = String::with_capacity(message.len().min(MAX_DIAGNOSTIC_MESSAGE_BYTES));
    for character in message.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if sanitized.len() + character.len_utf8() > MAX_DIAGNOSTIC_MESSAGE_BYTES {
            break;
        }
        sanitized.push(character);
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticLevel, DiagnosticLog, MAX_DIAGNOSTIC_ENTRIES, MAX_DIAGNOSTIC_MESSAGE_BYTES,
    };

    #[test]
    fn bounds_records_and_sanitizes_control_data() {
        let mut log = DiagnosticLog::default();
        log.record(DiagnosticLevel::Info, "test", "line\nsecret\tvalue");
        let snapshot = log.snapshot();
        assert!(snapshot.contains("0 info test: line secret value"));
        assert!(!snapshot.lines().any(|line| line == "secret"));

        for index in 0..(MAX_DIAGNOSTIC_ENTRIES + 4) {
            log.record(DiagnosticLevel::Warn, "test", index.to_string());
        }
        let snapshot = log.snapshot();
        assert!(!snapshot.contains("0 info test"));
        assert!(snapshot.contains("4 warn test: 3"));
        assert_eq!(snapshot.lines().count(), MAX_DIAGNOSTIC_ENTRIES);
    }

    #[test]
    fn truncates_one_record_without_splitting_utf8() {
        let mut log = DiagnosticLog::default();
        log.record(
            DiagnosticLevel::Error,
            "test",
            "é".repeat(MAX_DIAGNOSTIC_MESSAGE_BYTES),
        );
        let snapshot = log.snapshot();
        let message = snapshot.split_once(": ").expect("record message").1;
        assert!(message.trim_end().len() <= MAX_DIAGNOSTIC_MESSAGE_BYTES);
        assert!(message.trim_end().chars().all(|character| character == 'é'));
    }

    #[test]
    fn clear_removes_records_but_keeps_sequence_monotonic() {
        let mut log = DiagnosticLog::default();
        log.record(DiagnosticLevel::Info, "test", "before");
        log.clear();
        log.record(DiagnosticLevel::Info, "test", "after");
        assert_eq!(log.snapshot(), "1 info test: after\n");
    }
}
