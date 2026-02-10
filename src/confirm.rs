use async_trait::async_trait;
use std::io::{self, Write};

use crate::error::{AthenaError, Result};

/// Frontend-agnostic confirmation trait.
/// CLI reads from stdin, Telegram sends inline keyboards, etc.
#[async_trait]
pub trait Confirmer: Send + Sync {
    async fn confirm(&self, action: &str) -> Result<bool>;
}

/// CLI confirmer: reads y/N from stdin, supports auto-approve mode.
pub struct CliConfirmer {
    pub auto_approve: bool,
}

#[async_trait]
impl Confirmer for CliConfirmer {
    async fn confirm(&self, action: &str) -> Result<bool> {
        if self.auto_approve {
            eprintln!("⚡ Auto-approved: {}", action);
            return Ok(true);
        }

        let action = action.to_string();
        tokio::task::spawn_blocking(move || {
            eprint!("\n⚠  Action: {}\n   Approve? [y/N] ", action);
            io::stderr().flush().unwrap();

            let mut input = String::new();
            io::stdin()
                .read_line(&mut input)
                .map_err(|e| AthenaError::Tool(format!("Failed to read input: {}", e)))?;

            let answer = input.trim().to_lowercase();
            if answer == "y" || answer == "yes" {
                Ok(true)
            } else {
                Err(AthenaError::Cancelled)
            }
        })
        .await
        .map_err(|e| AthenaError::Tool(format!("Confirmation task failed: {}", e)))?
    }
}

/// Check if a command matches any sensitive patterns
pub fn is_sensitive(cmd: &str, patterns: &[String]) -> bool {
    for pat in patterns {
        if let Ok(re) = regex::Regex::new(pat) {
            if re.is_match(cmd) {
                return true;
            }
        }
    }
    false
}
