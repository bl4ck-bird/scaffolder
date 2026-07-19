//! Hook orchestration: inline `when` evaluation, confirm-description assembly, and per-phase
//! execution (inline in declaration order, then folder scripts in lexical order). `pipeline`
//! calls into this module and only wires ports.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::domain::answer::{AnswerContext, ConditionEvaluator};
use crate::domain::hook::{Hook, HookRunner, HookScript};
use crate::domain::render::Renderer;

/// Keeps only active inline hooks, in declaration order. No `when` means active; a `when` is
/// evaluated with the same evaluator as question `when`.
pub fn collect_active_inline<'a>(
    hooks: &'a [Hook],
    ctx: &AnswerContext,
    evaluator: &dyn ConditionEvaluator,
) -> Result<Vec<&'a Hook>> {
    let mut active = Vec::with_capacity(hooks.len());
    for hook in hooks {
        let is_active = match &hook.when {
            Some(when) => evaluator.is_active(when, ctx)?,
            None => true,
        };
        if is_active {
            active.push(hook);
        }
    }
    Ok(active)
}

/// Description for the single pre-side-effect confirm gate. Lists before then after, each as
/// inline (declaration order) followed by folder scripts (lexical), one per line.
pub fn confirm_description(
    before_inline: &[&Hook],
    before_scripts: &[HookScript],
    after_inline: &[&Hook],
    after_scripts: &[HookScript],
) -> String {
    let mut lines = Vec::new();
    push_phase_lines(&mut lines, "before", before_inline, before_scripts);
    push_phase_lines(&mut lines, "after", after_inline, after_scripts);
    lines.join("\n")
}

fn push_phase_lines(
    lines: &mut Vec<String>,
    phase: &str,
    inline: &[&Hook],
    scripts: &[HookScript],
) {
    for hook in inline {
        lines.push(format!("{phase}: {}", sanitize_for_display(&hook.run)));
    }
    for script in scripts {
        lines.push(format!(
            "{phase}: run {}",
            sanitize_for_display(script_name(script))
        ));
    }
}

fn script_name(script: &HookScript) -> &str {
    match script {
        HookScript::Executable { name, .. } => name,
        HookScript::Template { name, .. } => name,
    }
}

/// The confirm prompt is the only guard before arbitrary code runs, so escape control
/// characters (CR, ANSI escapes, …) an untrusted template author might use to spoof the
/// terminal display. Printable and non-ASCII characters are preserved (`str::escape_default`
/// over-escapes those, so it is not used).
fn sanitize_for_display(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Runs one phase: inline (declaration order) first, then folder scripts (already lexical).
/// A `Template` script is rendered with `renderer`, then run via `run_rendered`.
pub fn run_phase(
    runner: &dyn HookRunner,
    renderer: &dyn Renderer,
    ctx: &AnswerContext,
    inline: &[&Hook],
    scripts: &[HookScript],
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    for hook in inline {
        runner.run_inline(&hook.run, cwd, env)?;
    }
    for script in scripts {
        match script {
            HookScript::Executable { path, .. } => runner.run_script_file(path, cwd, env)?,
            HookScript::Template { name, raw } => {
                let rendered = renderer.render_str(raw, ctx)?;
                runner.run_rendered(name, rendered.as_bytes(), cwd, env)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::answer::{ScaffolderBuiltins, build_context};
    use crate::domain::data::DataValue;
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn builtins() -> ScaffolderBuiltins {
        ScaffolderBuiltins {
            name: "demo".to_string(),
            target: PathBuf::from("/tmp/demo"),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            username: "tester".to_string(),
        }
    }

    fn ctx() -> AnswerContext {
        build_context(BTreeMap::new(), Some(DataValue::empty_table()), builtins())
    }

    fn hook(run: &str, when: Option<&str>) -> Hook {
        Hook {
            when: when.map(|w| w.to_string()),
            run: run.to_string(),
        }
    }

    struct FixedConditionEvaluator(std::collections::HashMap<String, bool>);
    impl ConditionEvaluator for FixedConditionEvaluator {
        fn is_active(&self, when: &str, _ctx: &AnswerContext) -> Result<bool> {
            Ok(*self.0.get(when).unwrap_or(&false))
        }
    }

    #[test]
    fn collect_active_inline_keeps_no_when_and_active_when_in_declared_order() {
        let hooks = vec![hook("a", None), hook("b", Some("gate")), hook("c", None)];
        let evaluator = FixedConditionEvaluator([("gate".to_string(), true)].into_iter().collect());

        let active = collect_active_inline(&hooks, &ctx(), &evaluator).expect("collect");

        let runs: Vec<&str> = active.iter().map(|h| h.run.as_str()).collect();
        assert_eq!(runs, vec!["a", "b", "c"]);
    }

    #[test]
    fn collect_active_inline_drops_inactive_when() {
        let hooks = vec![hook("a", Some("gate")), hook("b", None)];
        let evaluator = FixedConditionEvaluator(std::collections::HashMap::new());

        let active = collect_active_inline(&hooks, &ctx(), &evaluator).expect("collect");

        let runs: Vec<&str> = active.iter().map(|h| h.run.as_str()).collect();
        assert_eq!(runs, vec!["b"]);
    }

    #[test]
    fn confirm_description_lists_before_then_after_inline_then_folder() {
        let before_a = hook("echo a", None);
        let after_a = hook("echo z", None);
        let before_inline = vec![&before_a];
        let after_inline = vec![&after_a];
        let before_scripts = vec![HookScript::Executable {
            name: "10-setup.sh".to_string(),
            path: PathBuf::from("/tpl/hooks/before/10-setup.sh"),
        }];

        let desc = confirm_description(&before_inline, &before_scripts, &after_inline, &[]);

        assert_eq!(
            desc,
            "before: echo a\nbefore: run 10-setup.sh\nafter: echo z"
        );
    }

    #[test]
    fn confirm_description_escapes_control_chars_in_hook_run() {
        let spoofed = hook("echo a\x1b[2K\rmalicious", None);
        let before_inline = vec![&spoofed];

        let desc = confirm_description(&before_inline, &[], &[], &[]);

        assert!(
            !desc.contains('\x1b'),
            "raw ESC must not reach the terminal: {desc:?}"
        );
        assert!(
            !desc.contains('\r'),
            "raw CR must not reach the terminal: {desc:?}"
        );
        assert!(
            desc.contains("\\u{1b}") || desc.contains("\\x1b"),
            "escaped ESC must be visible: {desc:?}"
        );
        assert!(desc.contains("\\r"), "escaped CR must be visible: {desc:?}");
    }

    #[test]
    fn confirm_description_escapes_control_chars_in_script_name() {
        let script = HookScript::Executable {
            name: "10-setup\x1b[2K\r.sh".to_string(),
            path: PathBuf::from("/tpl/hooks/before/10-setup.sh"),
        };

        let desc = confirm_description(&[], &[script], &[], &[]);

        assert!(
            !desc.contains('\x1b'),
            "raw ESC must not reach the terminal: {desc:?}"
        );
        assert!(
            !desc.contains('\r'),
            "raw CR must not reach the terminal: {desc:?}"
        );
    }

    struct RecordingRunner {
        calls: RefCell<Vec<String>>,
    }
    impl RecordingRunner {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
            }
        }
    }
    impl HookRunner for RecordingRunner {
        fn run_inline(
            &self,
            command: &str,
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            self.calls.borrow_mut().push(format!("inline:{command}"));
            Ok(())
        }
        fn run_script_file(
            &self,
            path: &Path,
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("script:{}", path.display()));
            Ok(())
        }
        fn run_rendered(
            &self,
            name: &str,
            content: &[u8],
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            self.calls.borrow_mut().push(format!(
                "rendered:{name}:{}",
                String::from_utf8_lossy(content)
            ));
            Ok(())
        }
    }

    struct FakeRenderer;
    impl Renderer for FakeRenderer {
        fn render_str(&self, template: &str, _context: &AnswerContext) -> Result<String> {
            Ok(format!("rendered:{template}"))
        }
    }

    #[test]
    fn run_phase_runs_inline_before_folder_scripts_in_order() {
        let a = hook("cmd-a", None);
        let b = hook("cmd-b", None);
        let inline = vec![&a, &b];
        let scripts = vec![
            HookScript::Executable {
                name: "z.sh".to_string(),
                path: PathBuf::from("/tpl/hooks/before/z.sh"),
            },
            HookScript::Template {
                name: "y.sh".to_string(),
                raw: "raw-content".to_string(),
            },
        ];
        let runner = RecordingRunner::new();

        run_phase(
            &runner,
            &FakeRenderer,
            &ctx(),
            &inline,
            &scripts,
            Path::new("/target"),
            &BTreeMap::new(),
        )
        .expect("run_phase");

        assert_eq!(
            *runner.calls.borrow(),
            vec![
                "inline:cmd-a".to_string(),
                "inline:cmd-b".to_string(),
                "script:/tpl/hooks/before/z.sh".to_string(),
                "rendered:y.sh:rendered:raw-content".to_string(),
            ]
        );
    }

    #[test]
    fn run_phase_propagates_inline_failure_and_stops() {
        struct FailingRunner;
        impl HookRunner for FailingRunner {
            fn run_inline(
                &self,
                _command: &str,
                _cwd: &Path,
                _env: &BTreeMap<String, String>,
            ) -> Result<()> {
                anyhow::bail!("boom")
            }
            fn run_script_file(
                &self,
                _path: &Path,
                _cwd: &Path,
                _env: &BTreeMap<String, String>,
            ) -> Result<()> {
                panic!("must not run script after inline failure");
            }
            fn run_rendered(
                &self,
                _name: &str,
                _content: &[u8],
                _cwd: &Path,
                _env: &BTreeMap<String, String>,
            ) -> Result<()> {
                panic!("must not run rendered after inline failure");
            }
        }

        let a = hook("cmd-a", None);
        let inline = vec![&a];
        let scripts = vec![HookScript::Executable {
            name: "z.sh".to_string(),
            path: PathBuf::from("/tpl/hooks/before/z.sh"),
        }];

        let result = run_phase(
            &FailingRunner,
            &FakeRenderer,
            &ctx(),
            &inline,
            &scripts,
            Path::new("/target"),
            &BTreeMap::new(),
        );

        assert!(result.is_err());
    }
}
