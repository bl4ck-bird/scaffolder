//! End-to-end apply of the `examples/rust-starter` template: exercises conditional skipping,
//! gitignore dedup, mode prefixes (+x), and verbatim passthrough (`${{ }}` preserved) via real `apply`.

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;

fn template_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/rust-starter")
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn apply_with_docker_and_ci_stacks_renders_conditional_files_and_dedups_gitignore() {
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template_root())
        .arg(&target)
        .arg("--name")
        .arg("demo")
        .arg("--answers")
        .arg("stacks=docker,ci")
        .arg("--defaults");

    cmd.assert().success();

    // core files present and rendered/verbatim as expected.
    assert!(
        target.join("Cargo.toml").exists(),
        "Cargo.toml must be written"
    );
    assert!(
        target.join("README.md").exists(),
        "README.md must be written"
    );
    assert!(
        target.join("src/main.rs").exists(),
        "src/main.rs must be written"
    );
    assert!(
        target.join(".gitignore").exists(),
        ".gitignore must be written"
    );
    assert!(
        target.join(".editorconfig").exists(),
        ".editorconfig must be written"
    );
    assert!(
        target.join("scripts/build.sh").exists(),
        "scripts/build.sh must be written"
    );

    let cargo_toml = fs::read_to_string(target.join("Cargo.toml")).expect("read Cargo.toml");
    assert!(
        cargo_toml.contains("name = \"demo\""),
        "Cargo.toml must render project name"
    );

    let main_rs = fs::read_to_string(target.join("src/main.rs")).expect("read src/main.rs");
    assert!(
        main_rs.contains("Hello, world!"),
        "src/main.rs must be copied verbatim"
    );

    // .gitignore: rust base pattern + docker fragment, deduplicated (no duplicate lines).
    let gitignore = fs::read_to_string(target.join(".gitignore")).expect("read .gitignore");
    assert!(
        gitignore.contains("/target"),
        ".gitignore must include the rust base pattern"
    );
    assert!(
        gitignore.contains(".env"),
        ".gitignore must include the docker fragment"
    );
    assert_eq!(
        count_occurrences(&gitignore, "/target\n"),
        1,
        "/target must appear exactly once after dedup: {gitignore:?}"
    );
    assert_eq!(
        count_occurrences(&gitignore, "*.log\n"),
        1,
        "*.log must appear exactly once after dedup: {gitignore:?}"
    );

    // docker/ci selected: Dockerfile and .github/workflows/ci.yml must be placed.
    assert!(
        target.join("Dockerfile").exists(),
        "Dockerfile must be written when docker is selected"
    );
    assert!(
        target.join(".github/workflows/ci.yml").exists(),
        ".github/workflows/ci.yml must be written when ci is selected"
    );

    // ci.yml is verbatim (no .jinja suffix): `${{ }}` must survive un-rendered.
    let ci_yml = fs::read_to_string(target.join(".github/workflows/ci.yml")).expect("read ci.yml");
    assert!(
        ci_yml.contains("${{ matrix.os }}"),
        "ci.yml must preserve GitHub Actions `${{{{ }}}}` syntax verbatim: {ci_yml:?}"
    );

    // scripts/build.sh: rendered (mode-prefixed .jinja) and executable.
    let build_sh = fs::read_to_string(target.join("scripts/build.sh")).expect("read build.sh");
    assert!(
        build_sh.contains("docker build"),
        "build.sh must render the docker-conditional block"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(target.join("scripts/build.sh"))
            .expect("stat build.sh")
            .permissions()
            .mode();
        assert_ne!(
            mode & 0o111,
            0,
            "scripts/build.sh must have an execute bit set"
        );
    }
}

#[test]
fn apply_with_docker_only_stack_renders_dockerfile_without_ci() {
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template_root())
        .arg(&target)
        .arg("--name")
        .arg("demo")
        .arg("--answers")
        .arg("stacks=docker")
        .arg("--defaults");

    cmd.assert().success();

    assert!(
        target.join("Dockerfile").exists(),
        "Dockerfile must be written when docker is selected alone"
    );
    assert!(
        !target.join(".github").exists(),
        ".github/ must be skipped when ci is not selected, even with docker selected"
    );

    let gitignore = fs::read_to_string(target.join(".gitignore")).expect("read .gitignore");
    assert!(
        gitignore.contains(".env"),
        ".gitignore must include the docker fragment when docker is selected: {gitignore:?}"
    );
}

#[test]
fn apply_with_ci_only_stack_renders_workflow_without_dockerfile() {
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template_root())
        .arg(&target)
        .arg("--name")
        .arg("demo")
        .arg("--answers")
        .arg("stacks=ci")
        .arg("--defaults");

    cmd.assert().success();

    assert!(
        target.join(".github/workflows/ci.yml").exists(),
        ".github/workflows/ci.yml must be written when ci is selected alone"
    );
    assert!(
        !target.join("Dockerfile").exists(),
        "Dockerfile must be skipped when docker is not selected, even with ci selected"
    );

    let gitignore = fs::read_to_string(target.join(".gitignore")).expect("read .gitignore");
    assert!(
        !gitignore.contains(".env"),
        ".gitignore must not include the docker fragment when docker is not selected: {gitignore:?}"
    );
}

#[test]
fn apply_with_no_stacks_skips_conditional_files_and_excludes_docker_fragment() {
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template_root())
        .arg(&target)
        .arg("--name")
        .arg("demo")
        .arg("--defaults");

    cmd.assert().success();

    assert!(
        target.join("Cargo.toml").exists(),
        "Cargo.toml must still be written"
    );
    assert!(
        !target.join("Dockerfile").exists(),
        "Dockerfile must be skipped when docker is not selected"
    );
    assert!(
        !target.join(".github").exists(),
        ".github/ must be skipped entirely when ci is not selected"
    );

    let gitignore = fs::read_to_string(target.join(".gitignore")).expect("read .gitignore");
    assert!(
        !gitignore.contains(".env"),
        ".gitignore must not include the docker fragment when docker is not selected: {gitignore:?}"
    );
    assert!(
        gitignore.contains("/target"),
        ".gitignore must still include the rust base pattern"
    );
}
