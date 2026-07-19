//! The `Renderer`, `PartialSource`, and `SyntaxChecker` ports.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::domain::answer::AnswerContext;

/// Renders a template string against an `AnswerContext`; implemented by infra via MiniJinja.
pub trait Renderer {
    fn render_str(&self, template: &str, context: &AnswerContext) -> Result<String>;
}

/// Port loading the `partials/` fragments. Because `{% include %}` pulls a partial by name, the
/// renderer has to have them all registered before rendering starts, so they are returned as a
/// map from name to source. Names are relative to `partials/` and `/`-separated; an absent
/// `partials/` directory yields an empty map.
pub trait PartialSource {
    fn load(&self, template_root: &Path) -> Result<BTreeMap<String, String>>;
}

/// Port compiling syntax only, without rendering or evaluation (for `template validate`
/// static checks). Strict-undefined variable references are not caught at parse time and are
/// out of scope, which avoids runtime-undefined false positives.
pub trait SyntaxChecker {
    fn check_template(&self, source: &str) -> Result<()>;
    fn check_expression(&self, source: &str) -> Result<()>;
}
