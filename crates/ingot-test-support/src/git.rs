use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use uuid::Uuid;

pub fn unique_temp_path(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{prefix}-{}", Uuid::now_v7()))
}

pub fn write_file(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write file");
}

pub fn run_git(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(path)
        .status()
        .expect("run git");
    assert!(status.success(), "git {:?} failed", args);
}

pub fn git_output(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git output");
    assert!(output.status.success(), "git {:?} failed", args);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn temp_git_repo(prefix: &str) -> PathBuf {
    let path = unique_temp_path(prefix);
    fs::create_dir_all(&path).expect("create temp repo dir");
    run_git(&path, &["init"]);
    run_git(&path, &["branch", "-M", "main"]);
    run_git(&path, &["config", "user.name", "Ingot Test"]);
    run_git(&path, &["config", "user.email", "ingot@example.com"]);
    write_file(&path.join("tracked.txt"), "initial");
    run_git(&path, &["add", "tracked.txt"]);
    run_git(&path, &["commit", "-m", "initial"]);
    path
}

#[cfg(test)]
mod tests {
    use super::{git_output, temp_git_repo};

    #[test]
    fn temp_git_repo_creates_committed_main_branch() {
        let repo = temp_git_repo("ingot-test-support-git");

        assert_eq!(git_output(&repo, &["branch", "--show-current"]), "main");
        assert!(!git_output(&repo, &["rev-parse", "HEAD"]).is_empty());
    }
}
