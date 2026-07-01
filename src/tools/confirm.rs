//! Interactive confirmation gate for high-impact, hard-to-reverse tool calls.
//!
//! Confirmation is layered *after* deterministic authorization. It can only
//! turn an already-authorized action into a denied one (or an allowed one once
//! the user says yes); it can never grant a capability that authorization
//! denied. This narrows the residual risk from ambiguous natural-language
//! intent without weakening the security boundary.

use std::io::{self, Write};

/// How the running session resolves a confirmation request.
#[derive(Clone)]
pub enum Confirmer {
    /// Prompt the user on the terminal and read a yes/no answer.
    Interactive,
    /// Deny without prompting. Used for one-shot CLI and image tasks where no
    /// interactive terminal is available to answer.
    AutoDeny,
    /// Allow without prompting. Test-only; never constructed by the binary.
    #[cfg(test)]
    AutoAllow,
}

impl Confirmer {
    pub fn interactive() -> Self {
        Self::Interactive
    }

    pub fn auto_deny() -> Self {
        Self::AutoDeny
    }

    /// Asks the user to approve `action`. Returns `true` only on an explicit
    /// affirmative answer. `action` should be a short, concrete description of
    /// the exact side effect (recipient, path, etc.).
    pub async fn confirm(&self, action: &str) -> bool {
        self.ask(&format!("Confirm {action}?")).await
    }

    /// Asks the user a yes/no `question` verbatim. Returns `true` only on an
    /// explicit affirmative answer. Non-interactive sessions answer no.
    pub async fn ask(&self, question: &str) -> bool {
        match self {
            Self::Interactive => prompt_terminal(question.to_owned()).await,
            Self::AutoDeny => false,
            #[cfg(test)]
            Self::AutoAllow => true,
        }
    }
}

/// Reads a single yes/no answer from the terminal. Runs the blocking stdin read
/// on a blocking thread so the async runtime is never stalled.
async fn prompt_terminal(question: String) -> bool {
    tokio::task::spawn_blocking(move || {
        print!("\n{question} [y/N] ");
        if io::stdout().flush().is_err() {
            return false;
        }
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    })
    .await
    .unwrap_or(false)
}
