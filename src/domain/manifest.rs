//! `Manifest`(questions·[data]·hooks)와 `ManifestSource` 포트.

use std::path::Path;

use anyhow::Result;

use crate::domain::question::Question;

/// `scaffold.toml` 파싱 결과. `data`/`hooks`는 이후 슬라이스에서 확장한다.
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub questions: Vec<Question>,
}

/// 경로로부터 `Manifest`를 로드하는 포트. infra가 TOML 파싱으로 구현한다.
pub trait ManifestSource {
    fn load(&self, path: &Path) -> Result<Manifest>;
}
