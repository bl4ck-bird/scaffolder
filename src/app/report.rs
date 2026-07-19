//! Formatting for the dry-run plan output.

use crate::app::pipeline::ApplyReport;

/// Renders an `ApplyReport` as human-readable lines (`would write: <rel> (rendered|verbatim)`).
pub fn format_plan(report: &ApplyReport) -> String {
    report
        .planned
        .iter()
        .map(|p| {
            let kind = if p.rendered { "rendered" } else { "verbatim" };
            format!("would write: {} ({kind})", p.rel)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::pipeline::PlannedWrite;
    use crate::domain::place::safe_rel_path;

    #[test]
    fn formats_rendered_and_verbatim_entries() {
        let report = ApplyReport {
            planned: vec![
                PlannedWrite {
                    rel: safe_rel_path("README.md").unwrap(),
                    rendered: true,
                    content: b"# demo".to_vec(),
                    mode: crate::domain::place::FileMode::base(),
                },
                PlannedWrite {
                    rel: safe_rel_path("src/main.rs").unwrap(),
                    rendered: false,
                    content: b"fn main(){}".to_vec(),
                    mode: crate::domain::place::FileMode::base(),
                },
            ],
        };

        let out = format_plan(&report);

        assert_eq!(
            out,
            "would write: README.md (rendered)\nwould write: src/main.rs (verbatim)"
        );
    }
}
