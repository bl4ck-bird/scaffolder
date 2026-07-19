//! Confirmation gates for hooks, overwrites, and external writes — `Confirmer`.

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::domain::hook::Confirmer;

/// A `Confirmer` that asks through a tty prompt. `force` applies only to overwrites: without it,
/// an overwrite is confirmed interactively and refused when there is no tty, because letting an
/// unapproved write through silently would be a security hole, so it must turn into an error
/// instead. `yes` applies only to hook confirmation and has no bearing on overwrite or
/// external-write decisions.
pub struct StdConfirmer {
    pub force: bool,
    pub interactive: bool,
    pub yes: bool,
}

impl StdConfirmer {
    pub fn new(force: bool, yes: bool) -> Self {
        Self {
            force,
            yes,
            interactive: io::stdin().is_terminal(),
        }
    }

    /// Refuses when non-interactive; prompts tty y/N when interactive. Does not consult
    /// `force` — the caller checks it first when needed.
    fn tty_prompt(&self, message: &str) -> bool {
        if !self.interactive {
            return false;
        }

        print!("{message} [y/N] ");
        if io::stdout().flush().is_err() {
            return false;
        }

        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() {
            return false;
        }

        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

impl Confirmer for StdConfirmer {
    fn confirm_hook(&self, description: &str) -> bool {
        if self.yes {
            return true;
        }
        self.tty_prompt(&format!("run hook: {description}?"))
    }

    fn confirm_overwrite(&self, path: &Path) -> bool {
        if self.force {
            return true;
        }
        self.tty_prompt(&format!("overwrite {}?", path.display()))
    }

    fn confirm_external_write(&self, path: &Path) -> bool {
        // A write that escapes containment can never be waved through by `--force`, which only
        // covers overwrites; it always needs interactive tty approval.
        self.tty_prompt(&format!("write outside target at {}?", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_does_not_approve_external_write() {
        let confirmer = StdConfirmer {
            force: true,
            interactive: false,
            yes: false,
        };

        assert!(!confirmer.confirm_external_write(Path::new("/outside/file.txt")));
    }

    #[test]
    fn force_still_approves_overwrite() {
        let confirmer = StdConfirmer {
            force: true,
            interactive: false,
            yes: false,
        };

        assert!(confirmer.confirm_overwrite(Path::new("/target/file.txt")));
    }

    #[test]
    fn force_does_not_approve_hook_confirm() {
        let confirmer = StdConfirmer {
            force: true,
            interactive: false,
            yes: false,
        };

        assert!(!confirmer.confirm_hook("run setup script"));
    }

    #[test]
    fn yes_bypasses_hook_confirm() {
        let confirmer = StdConfirmer {
            force: false,
            interactive: false,
            yes: true,
        };

        assert!(confirmer.confirm_hook("run setup script"));
    }

    #[test]
    fn non_interactive_without_yes_rejects_hook_confirm() {
        let confirmer = StdConfirmer {
            force: false,
            interactive: false,
            yes: false,
        };

        assert!(!confirmer.confirm_hook("run setup script"));
    }
}
