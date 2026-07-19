//! 훅·overwrite·외부쓰기 confirm 게이트 — `Confirmer`.

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::domain::hook::Confirmer;

/// tty 프롬프트 기반 `Confirmer`. `force`는 overwrite 전용 게이트다 — 대화형일 때만
/// 프롬프트하고 비대화형은 거부한다(미승인 write는 에러로 이어져야 하는 보안 표면).
/// `yes`는 훅 confirm 전용 우회다 — overwrite/외부쓰기에는 관여하지 않는다.
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

    /// 비대화형은 거부, 대화형은 tty y/N. `force`를 참조하지 않는다 — 호출부에서 필요하면 먼저 확인한다.
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
        // containment 이탈은 `--force`(overwrite 전용)로 우회 불가 — 항상 tty 승인이 필요하다.
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
