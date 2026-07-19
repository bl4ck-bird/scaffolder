//! The `Manifest` model and the `ManifestSource` port.

use std::path::Path;

use anyhow::Result;

use crate::domain::data::DataValue;
use crate::domain::hook::Hooks;
use crate::domain::question::Question;

/// Parsed `scaffold.toml`. `data` is the `[data]` section; `data/*.toml` files are overlaid
/// separately by `DataSource`.
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub questions: Vec<Question>,
    pub data: DataValue,
    pub hooks: Hooks,
}

/// Port loading a `Manifest` from a path; implemented by infra via TOML parsing.
pub trait ManifestSource {
    fn load(&self, path: &Path) -> Result<Manifest>;
}
