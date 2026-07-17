//! `scaffolder apply` e2e: 렌더/verbatim 배치, overwrite confirm, dry-run.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;

fn write_template(dir: &std::path::Path) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[questions]]
            name = "project"
            type = "string"
        "#,
    )
    .expect("write scaffold.toml");

    let files = dir.join("files");
    fs::create_dir_all(files.join("src")).expect("mkdir files/src");
    fs::write(files.join("README.md.jinja"), "# {{ project }}").expect("write README.md.jinja");
    fs::write(files.join("src/main.rs"), "fn main(){}").expect("write src/main.rs");
}

#[test]
fn apply_renders_and_writes_files() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo");

    cmd.assert().success();

    let readme = fs::read_to_string(target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "# demo");

    let main_rs = fs::read_to_string(target.join("src/main.rs")).expect("read src/main.rs");
    assert_eq!(main_rs, "fn main(){}");
}

#[test]
fn apply_without_force_fails_on_existing_destination_noninteractively() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");
    fs::create_dir_all(&target).expect("mkdir target");
    fs::write(target.join("README.md"), "existing").expect("seed existing README.md");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo");

    cmd.assert().failure();

    let readme = fs::read_to_string(target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "existing", "unapproved overwrite must not happen");
}

#[test]
fn apply_with_force_overwrites_existing_destination() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");
    fs::create_dir_all(&target).expect("mkdir target");
    fs::write(target.join("README.md"), "existing").expect("seed existing README.md");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo")
        .arg("--force");

    cmd.assert().success();

    let readme = fs::read_to_string(target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "# demo");
}

#[test]
fn apply_dry_run_does_not_write_and_prints_plan() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo")
        .arg("--dry-run");

    cmd.assert().success().stdout(contains("README.md"));

    assert!(!target.exists(), "dry-run must not create the target directory");
}
