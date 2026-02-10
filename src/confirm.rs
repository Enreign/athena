use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{AthenaError, Result};

static AUTO_APPROVE: AtomicBool = AtomicBool::new(false);

/// Set auto-approve mode (skip all confirmation prompts)
pub fn set_auto_approve(enabled: bool) {
    AUTO_APPROVE.store(enabled, Ordering::Relaxed);
}

/// Prompt user for confirmation of a sensitive action.
/// Returns Ok(true) if approved, Err(Cancelled) if denied.
pub fn confirm(action: &str) -> Result<bool> {
    if AUTO_APPROVE.load(Ordering::Relaxed) {
        eprintln!("⚡ Auto-approved: {}", action);
        return Ok(true);
    }

    eprint!("\n⚠  Action: {}\n   Approve? [y/N] ", action);
    io::stderr().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input)
        .map_err(|e| AthenaError::Tool(format!("Failed to read input: {}", e)))?;

    let answer = input.trim().to_lowercase();
    if answer == "y" || answer == "yes" {
        Ok(true)
    } else {
        Err(AthenaError::Cancelled)
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
