//! Lexing and parsing for Rune's command language.
//!
//! Parsing produces an execution plan. It never accesses the host filesystem
//! and never starts a process; those responsibilities belong to higher layers.

use std::fmt::{Display, Formatter};

/// A fragment of a shell word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordPart {
    /// Text that does not require environment expansion.
    Literal(String),
    /// An environment variable reference such as `$HOME` or `${HOME}`.
    Variable(String),
}

/// A shell word, preserving enough information to expand variables later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    parts: Vec<WordPart>,
}

impl Word {
    fn new(parts: Vec<WordPart>) -> Self {
        Self { parts }
    }

    /// Returns the fragments that make up this word.
    #[must_use]
    pub fn parts(&self) -> &[WordPart] {
        &self.parts
    }

    /// Returns the literal representation when no expansion is involved.
    #[must_use]
    pub fn literal_value(&self) -> Option<String> {
        let mut value = String::new();
        for part in &self.parts {
            match part {
                WordPart::Literal(text) => value.push_str(text),
                WordPart::Variable(_) => return None,
            }
        }
        Some(value)
    }
}

/// Operators recognized by the initial shell grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Word,
    Pipe,
    Sequence,
    And,
    RedirectStdout { append: bool },
    RedirectStderr { append: bool },
    RedirectStdin,
}

/// A parsed stream redirection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redirection {
    Stdin { path: Word },
    Stdout { path: Word, append: bool },
    Stderr { path: Word, append: bool },
}

/// One command and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    pub program: Word,
    pub arguments: Vec<Word>,
    pub redirections: Vec<Redirection>,
}

/// Commands connected by pipes. The first command receives the pipeline's
/// external stdin and each following command receives the previous stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelinePlan {
    pub commands: Vec<CommandPlan>,
}

/// Connector between adjacent pipelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector {
    /// Always execute the next pipeline.
    Sequence,
    /// Execute the next pipeline only when the previous one succeeded.
    And,
}

/// Complete parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub pipelines: Vec<PipelinePlan>,
    pub connectors: Vec<Connector>,
}

impl ExecutionPlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pipelines.is_empty()
    }
}

/// Errors raised before a command can be executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnclosedSingleQuote,
    UnclosedDoubleQuote,
    TrailingEscape,
    InvalidVariable(String),
    UnexpectedToken(String),
    MissingRedirectionTarget,
    EmptyPipeline,
}

impl Display for ParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnclosedSingleQuote => formatter.write_str("unclosed single quote"),
            Self::UnclosedDoubleQuote => formatter.write_str("unclosed double quote"),
            Self::TrailingEscape => formatter.write_str("trailing escape"),
            Self::InvalidVariable(name) => write!(formatter, "invalid variable reference: ${name}"),
            Self::UnexpectedToken(token) => write!(formatter, "unexpected token {token}"),
            Self::MissingRedirectionTarget => {
                formatter.write_str("redirection is missing a target")
            }
            Self::EmptyPipeline => formatter.write_str("pipeline has no command"),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteMode {
    Normal,
    Single,
    Double,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Lexeme {
    Word(Word),
    Operator(Token),
}

fn append_literal(parts: &mut Vec<WordPart>, text: impl Into<String>) {
    let text = text.into();
    if text.is_empty() {
        return;
    }
    if let Some(WordPart::Literal(previous)) = parts.last_mut() {
        previous.push_str(&text);
    } else {
        parts.push(WordPart::Literal(text));
    }
}

fn push_variable(parts: &mut Vec<WordPart>, name: String) {
    parts.push(WordPart::Variable(name));
}

fn parse_variable(input: &[char], index: &mut usize) -> Result<Option<String>, ParseError> {
    let next = input.get(*index + 1).copied();
    match next {
        Some('{') => {
            let name_start = *index + 2;
            let mut cursor = name_start;
            while let Some(character) = input.get(cursor) {
                if *character == '}' {
                    let name: String = input[name_start..cursor].iter().collect();
                    if name.is_empty()
                        || !name.chars().next().is_some_and(|character| {
                            character == '_' || character.is_ascii_alphabetic()
                        })
                        || !name
                            .chars()
                            .all(|character| character == '_' || character.is_ascii_alphanumeric())
                    {
                        return Err(ParseError::InvalidVariable(name));
                    }
                    *index = cursor;
                    return Ok(Some(name));
                }
                cursor += 1;
            }
            Err(ParseError::InvalidVariable(
                input[name_start..].iter().collect(),
            ))
        }
        Some('?') => {
            *index += 1;
            Ok(Some("?".to_string()))
        }
        Some(character) if character == '_' || character.is_ascii_alphabetic() => {
            let name_start = *index + 1;
            let mut cursor = name_start + 1;
            while let Some(character) = input.get(cursor) {
                if !(*character == '_' || character.is_ascii_alphanumeric()) {
                    break;
                }
                cursor += 1;
            }
            *index = cursor - 1;
            Ok(Some(input[name_start..cursor].iter().collect()))
        }
        _ => Ok(None),
    }
}

/// Tokenizes a command line while respecting quotes, escapes, variables, and
/// the supported operators.
///
/// # Errors
///
/// Returns a [`ParseError`] when the line contains an unclosed quote, a
/// dangling escape, or an invalid variable reference.
#[allow(clippy::too_many_lines)]
pub fn tokenize(input: &str) -> Result<Vec<(Token, Option<Word>)>, ParseError> {
    let characters: Vec<char> = input.chars().collect();
    let mut lexemes = Vec::new();
    let mut parts = Vec::new();
    let mut started = false;
    let mut mode = QuoteMode::Normal;
    let mut index = 0;

    let flush_word = |lexemes: &mut Vec<Lexeme>, parts: &mut Vec<WordPart>, started: &mut bool| {
        if *started {
            lexemes.push(Lexeme::Word(Word::new(std::mem::take(parts))));
            *started = false;
        }
    };

    while index < characters.len() {
        let character = characters[index];
        match mode {
            QuoteMode::Single => {
                if character == '\'' {
                    mode = QuoteMode::Normal;
                } else {
                    append_literal(&mut parts, character.to_string());
                }
                index += 1;
            }
            QuoteMode::Double => {
                if character == '"' {
                    mode = QuoteMode::Normal;
                    index += 1;
                } else if character == '\\' {
                    let next = characters
                        .get(index + 1)
                        .ok_or(ParseError::TrailingEscape)?;
                    if matches!(next, '"' | '\\' | '$' | '\n') {
                        append_literal(&mut parts, next.to_string());
                    } else {
                        append_literal(&mut parts, format!("\\{next}"));
                    }
                    started = true;
                    index += 2;
                } else if character == '$' {
                    if let Some(name) = parse_variable(&characters, &mut index)? {
                        push_variable(&mut parts, name);
                    } else {
                        append_literal(&mut parts, "$".to_string());
                    }
                    started = true;
                    index += 1;
                } else {
                    append_literal(&mut parts, character.to_string());
                    started = true;
                    index += 1;
                }
            }
            QuoteMode::Normal => {
                if character.is_whitespace() {
                    flush_word(&mut lexemes, &mut parts, &mut started);
                    index += 1;
                } else if character == '\'' {
                    mode = QuoteMode::Single;
                    started = true;
                    index += 1;
                } else if character == '"' {
                    mode = QuoteMode::Double;
                    started = true;
                    index += 1;
                } else if character == '\\' {
                    let next = characters
                        .get(index + 1)
                        .ok_or(ParseError::TrailingEscape)?;
                    append_literal(&mut parts, next.to_string());
                    started = true;
                    index += 2;
                } else if character == '$' {
                    if let Some(name) = parse_variable(&characters, &mut index)? {
                        push_variable(&mut parts, name);
                    } else {
                        append_literal(&mut parts, "$".to_string());
                    }
                    started = true;
                    index += 1;
                } else {
                    let operator = match character {
                        '|' => Some((Token::Pipe, 1)),
                        ';' => Some((Token::Sequence, 1)),
                        '<' => Some((Token::RedirectStdin, 1)),
                        '>' => {
                            if characters.get(index + 1) == Some(&'>') {
                                Some((Token::RedirectStdout { append: true }, 2))
                            } else {
                                Some((Token::RedirectStdout { append: false }, 1))
                            }
                        }
                        '&' if characters.get(index + 1) == Some(&'&') => Some((Token::And, 2)),
                        '2' if matches!(characters.get(index + 1), Some('>')) => {
                            if characters.get(index + 2) == Some(&'>') {
                                Some((Token::RedirectStderr { append: true }, 3))
                            } else {
                                Some((Token::RedirectStderr { append: false }, 2))
                            }
                        }
                        _ => None,
                    };

                    if let Some((token, width)) = operator {
                        flush_word(&mut lexemes, &mut parts, &mut started);
                        lexemes.push(Lexeme::Operator(token));
                        index += width;
                    } else {
                        append_literal(&mut parts, character.to_string());
                        started = true;
                        index += 1;
                    }
                }
            }
        }
    }

    match mode {
        QuoteMode::Normal => flush_word(&mut lexemes, &mut parts, &mut started),
        QuoteMode::Single => return Err(ParseError::UnclosedSingleQuote),
        QuoteMode::Double => return Err(ParseError::UnclosedDoubleQuote),
    }

    Ok(lexemes
        .into_iter()
        .map(|lexeme| match lexeme {
            Lexeme::Word(word) => (Token::Word, Some(word)),
            Lexeme::Operator(token) => (token, None),
        })
        .collect())
}

fn token_name(token: Token) -> String {
    match token {
        Token::Word => "word".to_string(),
        Token::Pipe => "|".to_string(),
        Token::Sequence => ";".to_string(),
        Token::And => "&&".to_string(),
        Token::RedirectStdout { append: false } => ">".to_string(),
        Token::RedirectStdout { append: true } => ">>".to_string(),
        Token::RedirectStderr { append: false } => "2>".to_string(),
        Token::RedirectStderr { append: true } => "2>>".to_string(),
        Token::RedirectStdin => "<".to_string(),
    }
}

/// Parses a command line into pipelines, redirections, and sequencing.
///
/// # Errors
///
/// Returns a [`ParseError`] when operators are not connected to valid
/// commands or redirection targets.
#[allow(clippy::too_many_lines)]
pub fn parse(input: &str) -> Result<ExecutionPlan, ParseError> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Ok(ExecutionPlan {
            pipelines: Vec::new(),
            connectors: Vec::new(),
        });
    }

    let mut pipelines = Vec::new();
    let mut connectors = Vec::new();
    let mut index = 0;

    loop {
        let mut commands = Vec::new();
        loop {
            let Some((token, word)) = tokens.get(index) else {
                if commands.is_empty() {
                    return Err(ParseError::EmptyPipeline);
                }
                break;
            };
            let Some(program) = (if *token == Token::Word {
                word.clone()
            } else {
                None
            }) else {
                return Err(ParseError::UnexpectedToken(token_name(*token)));
            };
            index += 1;
            let mut arguments = Vec::new();
            let mut redirections = Vec::new();

            while let Some((token, word)) = tokens.get(index) {
                match token {
                    Token::Word => {
                        arguments.push(word.clone().ok_or(ParseError::EmptyPipeline)?);
                        index += 1;
                    }
                    Token::RedirectStdin => {
                        index += 1;
                        let target = tokens
                            .get(index)
                            .and_then(|(_, word)| word.clone())
                            .ok_or(ParseError::MissingRedirectionTarget)?;
                        redirections.push(Redirection::Stdin { path: target });
                        index += 1;
                    }
                    Token::RedirectStdout { append } => {
                        let append = *append;
                        index += 1;
                        let target = tokens
                            .get(index)
                            .and_then(|(_, word)| word.clone())
                            .ok_or(ParseError::MissingRedirectionTarget)?;
                        redirections.push(Redirection::Stdout {
                            path: target,
                            append,
                        });
                        index += 1;
                    }
                    Token::RedirectStderr { append } => {
                        let append = *append;
                        index += 1;
                        let target = tokens
                            .get(index)
                            .and_then(|(_, word)| word.clone())
                            .ok_or(ParseError::MissingRedirectionTarget)?;
                        redirections.push(Redirection::Stderr {
                            path: target,
                            append,
                        });
                        index += 1;
                    }
                    Token::Pipe | Token::Sequence | Token::And => break,
                }
            }

            commands.push(CommandPlan {
                program,
                arguments,
                redirections,
            });

            match tokens.get(index).map(|(token, _)| *token) {
                Some(Token::Pipe) => {
                    index += 1;
                    if tokens.get(index).is_none() {
                        return Err(ParseError::EmptyPipeline);
                    }
                }
                _ => break,
            }
        }

        pipelines.push(PipelinePlan { commands });
        match tokens.get(index).map(|(token, _)| *token) {
            Some(Token::Sequence) => {
                index += 1;
                if tokens.get(index).is_some() {
                    connectors.push(Connector::Sequence);
                } else {
                    break;
                }
            }
            Some(Token::And) => {
                index += 1;
                if tokens.get(index).is_none() {
                    return Err(ParseError::UnexpectedToken("&&".to_string()));
                }
                connectors.push(Connector::And);
            }
            Some(token) => return Err(ParseError::UnexpectedToken(token_name(token))),
            None => break,
        }
    }

    Ok(ExecutionPlan {
        pipelines,
        connectors,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse, tokenize, Connector, Redirection, Token, WordPart};

    #[test]
    fn tokenizes_quotes_and_variables_without_losing_word_boundaries() {
        let tokens = tokenize(r#"echo "hello world" '$HOME' $HOME"#).expect("valid command");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].0, Token::Word);
        assert_eq!(
            tokens[1].1.as_ref().expect("echo argument").parts(),
            &[WordPart::Literal("hello world".to_string())]
        );
        assert_eq!(
            tokens[2]
                .1
                .as_ref()
                .expect("single quoted argument")
                .parts(),
            &[WordPart::Literal("$HOME".to_string())]
        );
        assert_eq!(
            tokens[3].1.as_ref().expect("expanded argument").parts(),
            &[WordPart::Variable("HOME".to_string())]
        );
    }

    #[test]
    fn parses_pipeline_redirections_and_conditionals() {
        let plan = parse("echo hi | cat > out && pwd 2>> errors; history").expect("valid plan");
        assert_eq!(plan.pipelines.len(), 3);
        assert_eq!(plan.connectors, vec![Connector::And, Connector::Sequence]);
        assert_eq!(plan.pipelines[0].commands.len(), 2);
        assert_eq!(
            plan.pipelines[1].commands[0].redirections,
            vec![Redirection::Stderr {
                path: super::Word::new(vec![WordPart::Literal("errors".to_string())]),
                append: true
            }]
        );
    }

    #[test]
    fn rejects_unclosed_quotes_and_dangling_operators() {
        assert!(parse("echo 'unfinished").is_err());
        assert!(parse("echo hi |").is_err());
        assert!(parse("echo hi &&").is_err());
    }
}
