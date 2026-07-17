//! 훅·overwrite·외부쓰기 confirm 게이트 — `Confirmer`.

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::domain::hook::Confirmer;

/// tty 프롬프트 기반 `Confirmer`. `force`가 있으면 항상 승인, 없으면 대화형일 때만 프롬프트하고
/// 비대화형은 거부한다(§1.9-5, §1.10 — 미승인 write는 에러로 이어져야 하는 보안 표면).
pub struct StdConfirmer {
    pub force: bool,
    pub interactive: bool,
}

impl StdConfirmer {
    pub fn new(force: bool) -> Self {
        Self {
            force,
            interactive: io::stdin().is_terminal(),
        }
    }

    fn prompt(&self, message: &str) -> bool {
        if self.force {
            return true;
        }
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
    fn confirm_hook(&self, _description: &str) -> bool {
        // S1은 훅을 실행하지 않는다(M4에서 실제 confirm 프롬프트로 대체).
        self.force || self.interactive
    }

    fn confirm_overwrite(&self, path: &Path) -> bool {
        self.prompt(&format!("overwrite {}?", path.display()))
    }

    fn confirm_external_write(&self, path: &Path) -> bool {
        self.prompt(&format!("write outside target at {}?", path.display()))
    }
}
