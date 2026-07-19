//! File-name grammar parsing: `[<mode_>]<name>[.jinja]` (basename only; directory
//! names are literal).

use anyhow::{Result, bail};

/// Unix-only mode prefix. Stackable (e.g. `executable_private_`) and order-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Executable,
    Private,
    Readonly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub output_base: String,
    pub render: bool,
    pub modes: Vec<Mode>,
}

const MODE_PREFIXES: [(&str, Mode); 3] = [
    ("executable_", Mode::Executable),
    ("private_", Mode::Private),
    ("readonly_", Mode::Readonly),
];

pub fn parse_file_name(name: &str) -> Result<ParsedName> {
    let mut rest = name;
    let mut modes = Vec::new();
    'prefixes: loop {
        for (prefix, mode) in MODE_PREFIXES {
            if let Some(stripped) = rest.strip_prefix(prefix) {
                modes.push(mode);
                rest = stripped;
                continue 'prefixes;
            }
        }
        break;
    }

    let (output_base, render) = match rest.strip_suffix(".jinja") {
        Some(stripped) => (stripped.to_string(), true),
        None => (rest.to_string(), false),
    };

    if output_base.is_empty() {
        bail!("file name {name:?} has empty basename after stripping mode prefixes/.jinja suffix");
    }

    Ok(ParsedName {
        output_base,
        render,
        modes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_jinja_suffix_and_marks_render() {
        let parsed = parse_file_name("README.md.jinja").unwrap();
        assert_eq!(parsed.output_base, "README.md");
        assert!(parsed.render);
        assert!(parsed.modes.is_empty());
    }

    #[test]
    fn verbatim_without_jinja_suffix() {
        let parsed = parse_file_name("main.rs").unwrap();
        assert_eq!(parsed.output_base, "main.rs");
        assert!(!parsed.render);
    }

    #[test]
    fn strips_only_one_jinja_suffix() {
        let parsed = parse_file_name("foo.jinja.jinja").unwrap();
        assert_eq!(parsed.output_base, "foo.jinja");
        assert!(parsed.render);
    }

    #[test]
    fn parses_executable_mode_prefix() {
        let parsed = parse_file_name("executable_build.sh.jinja").unwrap();
        assert_eq!(parsed.output_base, "build.sh");
        assert!(parsed.render);
        assert_eq!(parsed.modes, vec![Mode::Executable]);
    }

    #[test]
    fn stacks_mode_prefixes_in_encountered_order() {
        let parsed = parse_file_name("executable_readonly_run.sh.jinja").unwrap();
        assert_eq!(parsed.output_base, "run.sh");
        assert!(parsed.render);
        assert_eq!(parsed.modes, vec![Mode::Executable, Mode::Readonly]);
    }

    #[test]
    fn underscore_in_basename_is_not_a_mode_prefix() {
        let parsed = parse_file_name("my_file.txt").unwrap();
        assert_eq!(parsed.output_base, "my_file.txt");
        assert!(!parsed.render);
        assert!(parsed.modes.is_empty());
    }

    #[test]
    fn empty_basename_after_strip_is_error() {
        assert!(parse_file_name(".jinja").is_err());
    }
}
