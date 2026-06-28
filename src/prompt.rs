use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
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

impl Highlighter for SlashHelper {}
impl Validator for SlashHelper {}
impl Helper for SlashHelper {}

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
}
