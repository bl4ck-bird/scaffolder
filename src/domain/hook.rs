//! `Hook`, `HookPhase`(before/after)와 `HookSource`·`HookRunner`·`Confirmer` 포트
//! (훅·overwrite·외부쓰기 confirm 겸용).

use std::path::Path;

/// 훅 실행·overwrite·외부쓰기 confirm 게이트. infra가 대화형으로 구현한다.
pub trait Confirmer {
    /// 훅 실행 전 confirm. `description`은 인라인 명령 또는 `run <file>` 표시.
    fn confirm_hook(&self, description: &str) -> bool;
    /// 기존 dest overwrite confirm(`--force`는 infra가 자동 승인으로 처리).
    fn confirm_overwrite(&self, path: &Path) -> bool;
    /// target 밖 쓰기 confirm(payload 외부 심링크 포함).
    fn confirm_external_write(&self, path: &Path) -> bool;
}
