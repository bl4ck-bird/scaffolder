//! `scaffolder template list`/`new` e2e: 정렬 출력, 빈 스토어 안내, 중복 name base 힌트,
//! 신규 템플릿 뼈대 생성.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;

/// `store_dir/name`에 조회 가능한 스토어 템플릿을 만든다.
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
    // 실제 개발자 `~/.scaffolder`가 열거에 새지 않도록 가짜 HOME으로 격리한다.
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

    cmd.assert().success().stdout(contains("No templates found."));
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
        .stdout(contains(scaffolder_home.path().to_str().expect("utf8 path")));
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

    // 생성물이 실제로 유효한 템플릿인지 apply 파이프라인(dry-run)에 태워 검증한다.
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
