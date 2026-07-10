use std::borrow::Cow;

use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Context, Helper};

use crate::ui;

pub struct SlashHelper;

impl Completer for SlashHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        position: usize,
        _context: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let prefix = &line[..position];
        if !prefix.starts_with('/') || prefix.chars().any(char::is_whitespace) {
            return Ok((0, Vec::new()));
        }
        Ok((0, slash_candidates(prefix)))
    }
}

impl Hinter for SlashHelper {
    type Hint = String;
}

impl Highlighter for SlashHelper {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        Cow::Owned(ui::highlight_prompt(prompt))
    }
}

impl Validator for SlashHelper {
    fn validate(&self, context: &mut ValidationContext<'_>) -> rustyline::Result<ValidationResult> {
        if is_incomplete(context.input()) {
            Ok(ValidationResult::Incomplete)
        } else {
            Ok(ValidationResult::Valid(None))
        }
    }
}

impl Helper for SlashHelper {}

/// Decides whether the current input should keep accepting more lines. Input is
/// incomplete when a line ends with a backslash continuation or an odd number of
/// triple-backtick code fences has been opened.
pub(crate) fn is_incomplete(input: &str) -> bool {
    let trimmed = input.trim_end_matches(['\r', '\n']);
    // Trailing backslash on the final line requests another line. A doubled
    // backslash at the very end is treated as a literal and does not continue.
    let trailing_backslashes = trimmed
        .chars()
        .rev()
        .take_while(|&character| character == '\\')
        .count();
    if trailing_backslashes % 2 == 1 {
        return true;
    }
    // An unclosed fenced code block keeps reading.
    let fences = input.lines().filter(|line| {
        let start = line.trim_start();
        start.starts_with("```") || start.starts_with("~~~")
    });
    fences.count() % 2 == 1
}

/// Removes backslash line-continuations, joining the continued lines. Fenced or
/// plain multi-line text is otherwise preserved verbatim.
pub(crate) fn normalize_multiline(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut lines = input.lines().peekable();
    while let Some(line) = lines.next() {
        let continues =
            line.ends_with('\\') && line.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1;
        if continues && lines.peek().is_some() {
            out.push_str(&line[..line.len() - 1]);
        } else {
            out.push_str(line);
            if lines.peek().is_some() {
                out.push('\n');
            }
        }
    }
    out
}

fn slash_candidates(prefix: &str) -> Vec<Pair> {
    ui::COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(prefix))
        .map(|command| Pair {
            display: format!("{:<12} {}", command.name, command.description),
            replacement: command.name.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_describes_slash_commands() {
        let candidates = slash_candidates("/mo");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].replacement, "/model");
        assert!(candidates[0].display.contains("Select the active model"));
        assert!(slash_candidates("/unknown").is_empty());
    }

    #[test]
    fn detects_incomplete_continuations_and_fences() {
        assert!(!is_incomplete("single line"));
        assert!(is_incomplete("first line\\"));
        assert!(!is_incomplete("escaped literal\\\\"));
        assert!(is_incomplete("```bash"));
        assert!(!is_incomplete("```bash\nls -la\n```"));
        assert!(!is_incomplete("line one\nline two"));
    }

    #[test]
    fn joins_backslash_continuations() {
        assert_eq!(normalize_multiline("foo \\\nbar"), "foo bar");
        // A real newline without a continuation is preserved.
        assert_eq!(normalize_multiline("foo\nbar"), "foo\nbar");
        // A fenced block keeps its newlines.
        assert_eq!(normalize_multiline("```\ncode\n```"), "```\ncode\n```");
    }
}
