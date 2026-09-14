use crate::{usage, CommandContext, CommandOutput};
use rune_fs::{FsError, VirtualFileSystem};

const MAX_TEST_ARGUMENTS: usize = 128;
const MAX_TEST_OPERAND_BYTES: usize = 64 * 1024;

/// Evaluates the bounded `test` expression language without starting a host
/// process. The bracket spelling is registered separately so it can enforce
/// its closing `]` token.
pub(super) fn test_command(context: &mut CommandContext<'_>) -> CommandOutput {
    evaluate_test(context.args, context.fs, "test", false)
}

pub(super) fn bracket_command(context: &mut CommandContext<'_>) -> CommandOutput {
    evaluate_test(context.args, context.fs, "[", true)
}

fn evaluate_test(
    arguments: &[String],
    filesystem: &dyn VirtualFileSystem,
    command: &str,
    bracket: bool,
) -> CommandOutput {
    if arguments.len() > MAX_TEST_ARGUMENTS {
        return usage(
            command,
            &format!("expression exceeds {MAX_TEST_ARGUMENTS} arguments"),
        );
    }
    if arguments
        .iter()
        .any(|argument| argument.len() > MAX_TEST_OPERAND_BYTES)
    {
        return usage(
            command,
            &format!("operand exceeds {MAX_TEST_OPERAND_BYTES} bytes"),
        );
    }

    let expression = if bracket {
        let Some((closing, expression)) = arguments.split_last() else {
            return usage(command, "usage: [ EXPRESSION ]");
        };
        if closing != "]" {
            return usage(command, "missing closing ]");
        }
        expression
    } else {
        arguments
    };
    let mut parser = TestParser {
        arguments: expression,
        position: 0,
        filesystem,
    };
    match parser.parse() {
        Ok(value) => CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            status: i32::from(!value),
        },
        Err(error) => usage(command, &error),
    }
}

struct TestParser<'a> {
    arguments: &'a [String],
    position: usize,
    filesystem: &'a dyn VirtualFileSystem,
}

impl TestParser<'_> {
    fn parse(&mut self) -> Result<bool, String> {
        if self.arguments.is_empty() {
            return Ok(false);
        }
        let value = self.parse_or()?;
        if self.position == self.arguments.len() {
            Ok(value)
        } else {
            Err(format!(
                "unexpected argument: {}",
                self.arguments[self.position]
            ))
        }
    }

    fn parse_or(&mut self) -> Result<bool, String> {
        let mut value = self.parse_and()?;
        while self.consume("-o") {
            value |= self.parse_and()?;
        }
        Ok(value)
    }

    fn parse_and(&mut self) -> Result<bool, String> {
        let mut value = self.parse_not()?;
        while self.consume("-a") {
            value &= self.parse_not()?;
        }
        Ok(value)
    }

    fn parse_not(&mut self) -> Result<bool, String> {
        if self.consume("!") {
            Ok(!self.parse_not()?)
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<bool, String> {
        if self.consume("(") {
            let value = self.parse_or()?;
            if !self.consume(")") {
                return Err("missing closing )".to_string());
            }
            return Ok(value);
        }

        let Some(first) = self.peek().map(str::to_string) else {
            return Err("expression requires an operand".to_string());
        };
        if is_unary_operator(&first) {
            let operator = first;
            self.position += 1;
            let operand = self.take_operand(&operator)?.to_string();
            return self.evaluate_unary(&operator, &operand);
        }
        if self.arguments.len().saturating_sub(self.position) >= 3 {
            let operator = self.arguments[self.position + 1].clone();
            if is_binary_operator(&operator) {
                let left = first;
                self.position += 2;
                let right = self.take_operand(&operator)?.to_string();
                return evaluate_binary(&left, &operator, &right);
            }
        }
        self.position += 1;
        Ok(!first.is_empty())
    }

    fn evaluate_unary(&self, operator: &str, operand: &str) -> Result<bool, String> {
        match operator {
            "-n" => Ok(!operand.is_empty()),
            "-z" => Ok(operand.is_empty()),
            "-e" | "-f" | "-d" | "-L" | "-h" | "-s" => match self.filesystem.metadata(operand) {
                Ok(info) => Ok(match operator {
                    "-e" => true,
                    "-f" => !info.is_directory && !info.is_symlink,
                    "-d" => info.is_directory && !info.is_symlink,
                    "-L" | "-h" => info.is_symlink,
                    "-s" => info.size > 0,
                    _ => unreachable!("unary operator is validated above"),
                }),
                Err(FsError::NotFound(_)) => Ok(false),
                Err(error) => Err(format!("cannot inspect {operand}: {error}")),
            },
            _ => Err(format!("unsupported unary operator: {operator}")),
        }
    }

    fn take_operand(&mut self, operator: &str) -> Result<&str, String> {
        let Some(operand) = self.arguments.get(self.position) else {
            return Err(format!("{operator} requires an operand"));
        };
        self.position += 1;
        Ok(operand)
    }

    fn consume(&mut self, value: &str) -> bool {
        if self.peek() == Some(value) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<&str> {
        self.arguments.get(self.position).map(String::as_str)
    }
}

fn is_unary_operator(value: &str) -> bool {
    matches!(value, "-n" | "-z" | "-e" | "-f" | "-d" | "-L" | "-h" | "-s")
}

fn is_binary_operator(value: &str) -> bool {
    matches!(
        value,
        "=" | "==" | "!=" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge"
    )
}

fn evaluate_binary(left: &str, operator: &str, right: &str) -> Result<bool, String> {
    match operator {
        "=" | "==" => Ok(left == right),
        "!=" => Ok(left != right),
        "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
            let left_number = left
                .parse::<i64>()
                .map_err(|_| format!("{left} is not an integer"))?;
            let right_number = right
                .parse::<i64>()
                .map_err(|_| format!("{right} is not an integer"))?;
            Ok(match operator {
                "-eq" => left_number == right_number,
                "-ne" => left_number != right_number,
                "-lt" => left_number < right_number,
                "-le" => left_number <= right_number,
                "-gt" => left_number > right_number,
                "-ge" => left_number >= right_number,
                _ => unreachable!("integer operator is validated above"),
            })
        }
        _ => Err(format!("unsupported binary operator: {operator}")),
    }
}
