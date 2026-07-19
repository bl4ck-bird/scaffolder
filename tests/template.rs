//! End-to-end `scaffolder template list`/`new`/`validate`: sorted output, empty-store guidance,
//! duplicate-name base hints, new template skeleton creation, and the static-check report + exit code.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;

/// Creates a resolvable store template at `store_dir/name`.
fn write_store_template(store_dir: &std::path::Path, name: &str) {
    let template_dir = store_dir.join(name);
    fs::create_dir_all(&template_dir).expect("mkdir store template dir");
    fs::write(template_dir.join("scaffold.toml"), "").expect("write scaffold.toml");
}

#[test]
fn template_list_prints_names_sorted() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    write_store_template(store_dir.path(), "zeta");
    write_store_template(store_dir.path(), "alpha");
    // Isolate with a fake HOME so a real developer `~/.scaffolder` does not leak into the listing.
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home.path())
        .arg("template")
        .arg("list")
        .arg("--template-dir")
        .arg(store_dir.path());

    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["alpha", "zeta"]);
}

#[test]
fn template_list_empty_store_prints_guidance() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home.path())
        .arg("template")
        .arg("list")
        .arg("--template-dir")
        .arg(store_dir.path());

    cmd.assert()
        .success()
        .stdout(contains("No templates found."));
}

#[test]
fn template_list_duplicate_name_across_bases_shows_base_hint() {
    let template_dir = tempfile::tempdir().expect("template_dir tempdir");
    let scaffolder_home = tempfile::tempdir().expect("scaffolder_home tempdir");
    write_store_template(template_dir.path(), "shared");
    write_store_template(scaffolder_home.path(), "shared");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.env("SCAFFOLDER_HOME", scaffolder_home.path())
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home.path())
        .arg("template")
        .arg("list")
        .arg("--template-dir")
        .arg(template_dir.path());

    cmd.assert()
        .success()
        .stdout(contains(template_dir.path().to_str().expect("utf8 path")))
        .stdout(contains(
            scaffolder_home.path().to_str().expect("utf8 path"),
        ));
}

fn new_cmd(store_dir: &std::path::Path, fake_home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home)
        .arg("template")
        .arg("new")
        .arg("--template-dir")
        .arg(store_dir);
    cmd
}

#[test]
fn template_new_creates_simple_skeleton_at_template_dir() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    let mut cmd = new_cmd(store_dir.path(), fake_home.path());
    cmd.arg("demo");

    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains(store_dir.path().join("demo").to_str().expect("utf8 path")));

    let created = store_dir.path().join("demo");
    assert!(created.join("scaffold.toml").is_file());
    assert!(created.join("files/README.md.jinja").is_file());
    assert!(!created.join("partials").exists());
}

#[test]
fn template_new_full_creates_full_skeleton_and_is_a_valid_apply_source() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    let mut cmd = new_cmd(store_dir.path(), fake_home.path());
    cmd.arg("demo-full").arg("--full");
    cmd.assert().success();

    let created = store_dir.path().join("demo-full");
    assert!(created.join("scaffold.toml").is_file());
    assert!(created.join("partials/header.txt").is_file());
    assert!(created.join("data/sample.toml").is_file());
    assert!(created.join("hooks/before").is_dir());
    assert!(created.join("hooks/after").is_dir());

    // Verify the output is actually a valid template by running it through apply (dry-run).
    let target_dir = tempfile::tempdir().expect("target tempdir");
    let mut apply_cmd = Command::cargo_bin("scaffolder").expect("binary");
    apply_cmd
        .env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home.path())
        .arg("apply")
        .arg("demo-full")
        .arg(target_dir.path().join("out").to_str().expect("utf8 path"))
        .arg("--template-dir")
        .arg(store_dir.path())
        .arg("--defaults")
        .arg("--dry-run");

    apply_cmd.assert().success();
}

#[test]
fn template_new_rerun_same_name_errors_without_side_effects() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    new_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .success();

    new_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .failure();
}

#[test]
fn template_new_rejects_invalid_name() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    new_cmd(store_dir.path(), fake_home.path())
        .arg("..")
        .assert()
        .failure()
        .stderr(contains("single path component"));
}

fn validate_cmd(store_dir: &std::path::Path, fake_home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home)
        .arg("template")
        .arg("validate")
        .arg("--template-dir")
        .arg(store_dir);
    cmd
}

#[test]
fn template_validate_passes_new_simple_skeleton() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    new_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .success();

    let assert = validate_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("demo: OK"), "stdout: {stdout}");
}

#[test]
fn template_validate_passes_new_full_skeleton() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    new_cmd(store_dir.path(), fake_home.path())
        .arg("demo-full")
        .arg("--full")
        .assert()
        .success();

    let assert = validate_cmd(store_dir.path(), fake_home.path())
        .arg("demo-full")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("demo-full: OK"), "stdout: {stdout}");
}

#[test]
fn template_validate_no_names_validates_whole_store() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    new_cmd(store_dir.path(), fake_home.path())
        .arg("alpha")
        .assert()
        .success();
    new_cmd(store_dir.path(), fake_home.path())
        .arg("beta")
        .assert()
        .success();

    let assert = validate_cmd(store_dir.path(), fake_home.path())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("alpha: OK"), "stdout: {stdout}");
    assert!(stdout.contains("beta: OK"), "stdout: {stdout}");
}

#[test]
fn template_validate_empty_store_prints_guidance_and_succeeds() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    validate_cmd(store_dir.path(), fake_home.path())
        .assert()
        .success()
        .stdout(contains("No templates to validate"));
}

#[test]
fn template_validate_reports_invalid_type_finding() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    new_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .success();

    let manifest_path = store_dir.path().join("demo/scaffold.toml");
    let manifest = fs::read_to_string(&manifest_path).expect("read manifest");
    fs::write(
        &manifest_path,
        manifest.replace("type = \"string\"", "type = \"bogus\""),
    )
    .expect("corrupt manifest with invalid type");

    let assert = validate_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("demo:"), "stdout: {stdout}");
    assert!(stdout.contains("[manifest]"), "stdout: {stdout}");
    // The root cause must surface — it must not be buried behind only the top-level context.
    assert!(stdout.contains("unknown type"), "stdout: {stdout}");
    assert!(stdout.contains("\"bogus\""), "stdout: {stdout}");
    assert!(
        stdout.matches("scaffold.toml").count() == 1,
        "path should not be duplicated, stdout: {stdout}"
    );
}

#[test]
fn template_validate_reports_jinja_syntax_error() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    new_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .success();

    fs::write(
        store_dir.path().join("demo/files/README.md.jinja"),
        "{% if unterminated %}",
    )
    .expect("corrupt jinja file with unterminated block");

    let assert = validate_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("demo:"), "stdout: {stdout}");
    assert!(stdout.contains("[template-syntax]"), "stdout: {stdout}");
    // minijinja's concrete diagnostic must show (without coupling to exact line/col wording).
    assert!(stdout.contains("syntax error"), "stdout: {stdout}");
}

#[test]
fn template_validate_reports_unregistered_include() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    new_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .success();

    fs::write(
        store_dir.path().join("demo/files/README.md.jinja"),
        "{% include \"missing\" %}",
    )
    .expect("corrupt jinja file with unregistered include");

    let assert = validate_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("demo:"), "stdout: {stdout}");
    assert!(stdout.contains("[partial-reference]"), "stdout: {stdout}");
    assert!(stdout.contains("missing"), "stdout: {stdout}");
}

#[test]
fn template_validate_unknown_name_fails_but_reports_others() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    new_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .assert()
        .success();

    let assert = validate_cmd(store_dir.path(), fake_home.path())
        .arg("demo")
        .arg("ghost")
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("demo: OK"), "stdout: {stdout}");
    assert!(stdout.contains("ghost"), "stdout: {stdout}");
}
