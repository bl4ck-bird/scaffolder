//! Static checks (`template validate`): schema, questions, `when`, file-name grammar, partial
//! references, and `.jinja` syntax. Not fail-fast — a manifest load failure is captured as one
//! finding, and independent payload-based checks (file name, syntax, source conflict, partial
//! references) still run. Uses only domain ports.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::domain::hook::Hooks;
use crate::domain::manifest::{Manifest, ManifestSource};
use crate::domain::name::parse_file_name;
use crate::domain::place::PayloadStore;
use crate::domain::render::{PartialSource, SyntaxChecker};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    Manifest,
    FileName,
    TemplateSyntax,
    WhenSyntax,
    SourceConflict,
    PartialReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: FindingKind,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub findings: Vec<Finding>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.findings.is_empty()
    }
}

/// The ports `validate_template` uses (a parameter object).
pub struct ValidatePorts<'a> {
    pub manifest_src: &'a dyn ManifestSource,
    pub partial_source: &'a dyn PartialSource,
    pub payload: &'a dyn PayloadStore,
    pub syntax: &'a dyn SyntaxChecker,
}

/// Runs the static template checks and returns every finding. `Err` is only for being unable
/// to run a check at all (e.g. an IO failure); template defects all go into `ValidationReport.findings`.
pub fn validate_template(template_root: &Path, ports: ValidatePorts) -> Result<ValidationReport> {
    let mut findings = Vec::new();

    let manifest_path = template_root.join("scaffold.toml");
    let manifest = match ports.manifest_src.load(&manifest_path) {
        Ok(manifest) => Some(manifest),
        Err(err) => {
            // The anyhow chain (`{err:#}`) already includes the manifest path, so prefixing it again would duplicate it.
            findings.push(Finding {
                kind: FindingKind::Manifest,
                message: format!("{err:#}"),
            });
            None
        }
    };

    if let Some(manifest) = &manifest {
        check_when_syntax(manifest, ports.syntax, &mut findings);
    }

    let partials = ports.partial_source.load(template_root)?;
    for (name, source) in &partials {
        check_template_source(
            &format!("partials/{name}"),
            source,
            &partials,
            ports.syntax,
            &mut findings,
        );
    }

    let files_root = template_root.join("files");
    let entries = ports.payload.list_entries(&files_root)?;

    let mut seen_outputs: BTreeMap<String, String> = BTreeMap::new();
    for entry in entries.iter().filter(|e| !e.is_dir) {
        let entry_display = entry.rel.to_string();

        let basename = match entry.rel.as_path().file_name().and_then(|n| n.to_str()) {
            Some(basename) => basename,
            None => {
                findings.push(Finding {
                    kind: FindingKind::FileName,
                    message: format!("{entry_display}: entry has no valid UTF-8 basename"),
                });
                continue;
            }
        };

        let parsed = match parse_file_name(basename) {
            Ok(parsed) => parsed,
            Err(err) => {
                findings.push(Finding {
                    kind: FindingKind::FileName,
                    message: format!("{entry_display}: {err}"),
                });
                continue;
            }
        };

        let out_rel_str = match entry.rel.as_path().parent() {
            Some(parent) if parent.as_os_str().is_empty() => parsed.output_base.clone(),
            Some(parent) => parent
                .join(&parsed.output_base)
                .to_string_lossy()
                .into_owned(),
            None => parsed.output_base.clone(),
        };

        if let Some(prior) = seen_outputs.get(&out_rel_str) {
            findings.push(Finding {
                kind: FindingKind::SourceConflict,
                message: format!(
                    "output path '{out_rel_str}' is produced by both '{prior}' and '{entry_display}'"
                ),
            });
        } else {
            seen_outputs.insert(out_rel_str, entry_display.clone());
        }

        if !parsed.render {
            continue;
        }

        let raw = ports.payload.read_content(&files_root, entry)?;
        match String::from_utf8(raw) {
            Ok(text) => {
                check_template_source(
                    &entry_display,
                    &text,
                    &partials,
                    ports.syntax,
                    &mut findings,
                );
            }
            Err(_) => {
                findings.push(Finding {
                    kind: FindingKind::TemplateSyntax,
                    message: format!(
                        "{entry_display}: marked for rendering but is not valid UTF-8"
                    ),
                });
            }
        }
    }

    Ok(ValidationReport { findings })
}

fn check_when_syntax(manifest: &Manifest, syntax: &dyn SyntaxChecker, findings: &mut Vec<Finding>) {
    for question in &manifest.questions {
        if let Some(when) = &question.when
            && let Err(err) = syntax.check_expression(when)
        {
            findings.push(Finding {
                kind: FindingKind::WhenSyntax,
                message: format!("question '{}' `when` syntax error: {err:#}", question.name),
            });
        }
    }
    check_hooks_when(&manifest.hooks, syntax, findings);
}

fn check_hooks_when(hooks: &Hooks, syntax: &dyn SyntaxChecker, findings: &mut Vec<Finding>) {
    let phases = hooks
        .before
        .iter()
        .map(|hook| ("before", hook))
        .chain(hooks.after.iter().map(|hook| ("after", hook)));
    for (phase, hook) in phases {
        if let Some(when) = &hook.when
            && let Err(err) = syntax.check_expression(when)
        {
            findings.push(Finding {
                kind: FindingKind::WhenSyntax,
                message: format!("{phase} hook `when` syntax error: {err:#}"),
            });
        }
    }
}

fn check_template_source(
    label: &str,
    source: &str,
    partials: &BTreeMap<String, String>,
    syntax: &dyn SyntaxChecker,
    findings: &mut Vec<Finding>,
) {
    if let Err(err) = syntax.check_template(source) {
        findings.push(Finding {
            kind: FindingKind::TemplateSyntax,
            message: format!("{label}: {err:#}"),
        });
    }

    for included in literal_includes(source) {
        if !partials.contains_key(&included) {
            findings.push(Finding {
                kind: FindingKind::PartialReference,
                message: format!("{label}: includes undefined partial '{included}'"),
            });
        }
    }
}

/// Extracts only literal includes of the form `{% include "name" %}` / `{% include 'name' %}`
/// (whitespace and `{%-`/`-%}` variants allowed). A dynamic include (variable, list, expression)
/// is naturally skipped since the first token after `include` is not a quote (avoiding false
/// positives — skip when unsure).
fn literal_includes(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel_open) = source[cursor..].find("{%") {
        let after_open = cursor + rel_open + 2;
        let content_start = source[after_open..]
            .strip_prefix('-')
            .map(|_| after_open + 1)
            .unwrap_or(after_open);

        let Some(rel_close) = source[content_start..].find("%}") else {
            break;
        };
        let close_at = content_start + rel_close;
        let content_end = if close_at > content_start && source.as_bytes()[close_at - 1] == b'-' {
            close_at - 1
        } else {
            close_at
        };

        let content = source[content_start..content_end].trim();
        if let Some(rest) = content.strip_prefix("include")
            && let Some(name) = parse_quoted_literal(rest.trim_start())
        {
            names.push(name);
        }

        cursor = close_at + 2;
    }

    names
}

fn parse_quoted_literal(s: &str) -> Option<String> {
    let quote = s.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &s[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::hook::Hook;
    use crate::domain::place::{DestStatus, FileMode, PayloadEntry, RelPath};
    use crate::domain::question::{Question, QuestionType};
    use anyhow::bail;
    use std::collections::HashMap;

    struct FakeManifestSource(Result<Manifest, String>);
    impl ManifestSource for FakeManifestSource {
        fn load(&self, _path: &Path) -> Result<Manifest> {
            match &self.0 {
                Ok(manifest) => Ok(manifest.clone()),
                Err(message) => bail!("{message}"),
            }
        }
    }

    struct FakePartialSource(BTreeMap<String, String>);
    impl FakePartialSource {
        fn empty() -> Self {
            Self(BTreeMap::new())
        }
    }
    impl PartialSource for FakePartialSource {
        fn load(&self, _template_root: &Path) -> Result<BTreeMap<String, String>> {
            Ok(self.0.clone())
        }
    }

    struct FakePayloadStore {
        entries: Vec<PayloadEntry>,
        contents: HashMap<String, Vec<u8>>,
    }
    impl FakePayloadStore {
        fn empty() -> Self {
            Self {
                entries: Vec::new(),
                contents: HashMap::new(),
            }
        }
    }
    impl PayloadStore for FakePayloadStore {
        fn list_entries(&self, _source_root: &Path) -> Result<Vec<PayloadEntry>> {
            Ok(self.entries.clone())
        }

        fn read_content(&self, _source_root: &Path, entry: &PayloadEntry) -> Result<Vec<u8>> {
            Ok(self
                .contents
                .get(&entry.rel.to_string())
                .cloned()
                .unwrap_or_default())
        }

        fn ensure_target(
            &self,
            _target_root: &Path,
        ) -> Result<crate::domain::place::TargetPreparation> {
            unreachable!("validate must never write to a target")
        }

        fn cleanup_target(&self, _target_root: &Path) -> Result<()> {
            unreachable!("validate must never clean up a target")
        }

        fn write_file(
            &self,
            _target_root: &Path,
            _rel: &RelPath,
            _content: &[u8],
            _mode: FileMode,
            _overwrite: bool,
        ) -> Result<()> {
            unreachable!("validate must never write to a target")
        }

        fn dest_status(&self, _target_root: &Path, _rel: &RelPath) -> Result<DestStatus> {
            unreachable!("validate must never inspect a target destination")
        }
    }

    /// Test `SyntaxChecker` that decides only by the presence of "BAD_TEMPLATE"/"BAD_EXPR"
    /// markers in `check_template`/`check_expression` — app-layer tests must not know real minijinja syntax.
    struct FakeSyntaxChecker;
    impl SyntaxChecker for FakeSyntaxChecker {
        fn check_template(&self, source: &str) -> Result<()> {
            if source.contains("BAD_TEMPLATE") {
                bail!("template syntax error");
            }
            Ok(())
        }

        fn check_expression(&self, source: &str) -> Result<()> {
            if source.contains("BAD_EXPR") {
                bail!("expression syntax error");
            }
            Ok(())
        }
    }

    fn entry(rel: &str) -> PayloadEntry {
        PayloadEntry {
            rel: crate::domain::place::safe_rel_path(rel).unwrap(),
            is_dir: false,
        }
    }

    fn question(name: &str, when: Option<&str>) -> Question {
        Question {
            name: name.to_string(),
            qtype: QuestionType::String,
            prompt: None,
            choices: Vec::new(),
            default: None,
            when: when.map(|w| w.to_string()),
            help: None,
        }
    }

    #[test]
    fn valid_template_has_no_findings() {
        let manifest = Manifest {
            questions: vec![question("project", None)],
            ..Default::default()
        };
        let store = FakePayloadStore {
            entries: vec![entry("README.md.jinja")],
            contents: HashMap::from([("README.md.jinja".to_string(), b"# {{ project }}".to_vec())]),
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert!(
            report.is_valid(),
            "expected no findings, got {:?}",
            report.findings
        );
    }

    #[test]
    fn manifest_load_failure_is_single_finding_and_skips_when_checks() {
        let store = FakePayloadStore::empty();

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Err("bad schema".to_string())),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].kind, FindingKind::Manifest);
    }

    #[test]
    fn manifest_load_failure_does_not_block_independent_payload_checks() {
        // Even without a manifest, payload-based checks like file-name grammar still run.
        let store = FakePayloadStore {
            entries: vec![entry("sub/.jinja")],
            contents: HashMap::new(),
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Err("bad schema".to_string())),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert_eq!(report.findings.len(), 2);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::Manifest)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::FileName)
        );
    }

    #[test]
    fn file_name_syntax_violation_is_finding() {
        let manifest = Manifest::default();
        let store = FakePayloadStore {
            entries: vec![entry("sub/.jinja")],
            contents: HashMap::new(),
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].kind, FindingKind::FileName);
    }

    #[test]
    fn template_syntax_violation_is_finding() {
        let manifest = Manifest::default();
        let store = FakePayloadStore {
            entries: vec![entry("main.rs.jinja")],
            contents: HashMap::from([("main.rs.jinja".to_string(), b"BAD_TEMPLATE".to_vec())]),
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].kind, FindingKind::TemplateSyntax);
    }

    #[test]
    fn verbatim_file_is_not_syntax_checked() {
        let manifest = Manifest::default();
        let store = FakePayloadStore {
            entries: vec![entry("main.rs")],
            contents: HashMap::from([("main.rs".to_string(), b"BAD_TEMPLATE".to_vec())]),
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert!(report.is_valid());
    }

    #[test]
    fn question_when_syntax_violation_is_finding() {
        let manifest = Manifest {
            questions: vec![question("feature", Some("BAD_EXPR"))],
            ..Default::default()
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &FakePayloadStore::empty(),
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].kind, FindingKind::WhenSyntax);
    }

    #[test]
    fn hook_when_syntax_violation_is_finding() {
        let manifest = Manifest {
            hooks: Hooks {
                before: vec![Hook {
                    when: Some("BAD_EXPR".to_string()),
                    run: "echo hi".to_string(),
                }],
                after: vec![],
            },
            ..Default::default()
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &FakePayloadStore::empty(),
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].kind, FindingKind::WhenSyntax);
    }

    #[test]
    fn source_conflict_on_same_output_path_is_finding_not_error() {
        let manifest = Manifest::default();
        let store = FakePayloadStore {
            entries: vec![entry("README.md"), entry("README.md.jinja")],
            contents: HashMap::from([
                ("README.md".to_string(), b"verbatim".to_vec()),
                ("README.md.jinja".to_string(), b"rendered".to_vec()),
            ]),
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed, source conflicts are findings not errors");

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].kind, FindingKind::SourceConflict);
    }

    #[test]
    fn unregistered_literal_include_is_finding() {
        let manifest = Manifest::default();
        let store = FakePayloadStore {
            entries: vec![entry("README.md.jinja")],
            contents: HashMap::from([(
                "README.md.jinja".to_string(),
                b"{% include \"missing\" %}".to_vec(),
            )]),
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].kind, FindingKind::PartialReference);
    }

    #[test]
    fn registered_literal_include_is_not_a_finding() {
        let manifest = Manifest::default();
        let store = FakePayloadStore {
            entries: vec![entry("README.md.jinja")],
            contents: HashMap::from([(
                "README.md.jinja".to_string(),
                b"{%- include 'greeting' -%}".to_vec(),
            )]),
        };
        let partials =
            FakePartialSource(BTreeMap::from([("greeting".to_string(), "hi".to_string())]));

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &partials,
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert!(report.is_valid());
    }

    #[test]
    fn dynamic_include_is_not_checked() {
        let manifest = Manifest::default();
        let store = FakePayloadStore {
            entries: vec![entry("README.md.jinja")],
            contents: HashMap::from([(
                "README.md.jinja".to_string(),
                b"{% include which_partial %}".to_vec(),
            )]),
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert!(
            report.is_valid(),
            "dynamic include must not be checked: {:?}",
            report.findings
        );
    }

    #[test]
    fn undefined_variable_reference_is_not_checked_by_syntax_checker() {
        // FakeSyntaxChecker only looks for the "BAD_TEMPLATE" marker, so this test guarantees the
        // aggregator has no logic of its own for undefined variables (it delegates entirely to the port).
        let manifest = Manifest::default();
        let store = FakePayloadStore {
            entries: vec![entry("README.md.jinja")],
            contents: HashMap::from([(
                "README.md.jinja".to_string(),
                b"{{ totally_undefined }}".to_vec(),
            )]),
        };

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &FakePartialSource::empty(),
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert!(report.is_valid());
    }

    #[test]
    fn partial_source_is_syntax_checked_and_scanned_for_includes() {
        let manifest = Manifest::default();
        let store = FakePayloadStore::empty();
        let partials = FakePartialSource(BTreeMap::from([(
            "header".to_string(),
            "BAD_TEMPLATE {% include \"missing\" %}".to_string(),
        )]));

        let report = validate_template(
            Path::new("/tpl"),
            ValidatePorts {
                manifest_src: &FakeManifestSource(Ok(manifest)),
                partial_source: &partials,
                payload: &store,
                syntax: &FakeSyntaxChecker,
            },
        )
        .expect("validate should succeed");

        assert_eq!(report.findings.len(), 2);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::TemplateSyntax)
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::PartialReference)
        );
    }
}
