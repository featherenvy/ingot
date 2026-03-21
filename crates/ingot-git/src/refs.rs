use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_ref::GitRef;
use ingot_domain::ports::{GitPortError, JobCompletionGitPort, TargetRefHoldError};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::time::{Instant, sleep, timeout, timeout_at};

use crate::commands::{GitCommandError, commit_exists, resolve_ref_oid};

const HOLD_READY_TIMEOUT: Duration = Duration::from_secs(5);
const HOLD_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const HOLD_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const TARGET_REF_LOCK_TIMEOUT_MESSAGE: &str = "timed out waiting for target ref lock";

#[derive(Debug, Clone, Default)]
pub struct GitJobCompletionPort;

#[derive(Debug)]
pub struct GitTargetRefHold {
    transaction: UpdateRefTransaction,
}

#[derive(Debug)]
struct UpdateRefTransaction {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
}

impl JobCompletionGitPort for GitJobCompletionPort {
    type Hold = GitTargetRefHold;

    async fn commit_exists(
        &self,
        repo_path: &Path,
        commit_oid: &CommitOid,
    ) -> Result<bool, GitPortError> {
        commit_exists(repo_path, commit_oid)
            .await
            .map_err(map_git_port_error)
    }

    async fn verify_and_hold_target_ref(
        &self,
        repo_path: &Path,
        target_ref: &GitRef,
        expected_oid: &CommitOid,
    ) -> Result<Self::Hold, TargetRefHoldError> {
        verify_and_hold_target_ref_with_timeout(
            repo_path,
            target_ref,
            expected_oid,
            HOLD_READY_TIMEOUT,
            HOLD_SHUTDOWN_TIMEOUT,
        )
        .await
    }

    async fn release_hold(&self, hold: Self::Hold) -> Result<(), GitPortError> {
        shutdown_update_ref_transaction(hold.transaction, HOLD_SHUTDOWN_TIMEOUT)
            .await
            .map_err(GitPortError::Internal)
    }
}

async fn verify_and_hold_target_ref_with_timeout(
    repo_path: &Path,
    target_ref: &GitRef,
    expected_oid: &CommitOid,
    ready_timeout: Duration,
    shutdown_timeout: Duration,
) -> Result<GitTargetRefHold, TargetRefHoldError> {
    let deadline = Instant::now() + ready_timeout;

    loop {
        let mut transaction = spawn_update_ref_transaction(repo_path).await?;
        write_update_ref_commands(
            transaction.stdin.as_mut().ok_or_else(|| {
                TargetRefHoldError::Internal("git update-ref missing stdin".into())
            })?,
            &format!(
                "start\nverify {} {}\nprepare\n",
                target_ref.as_str(),
                expected_oid.as_str()
            ),
        )
        .await?;

        let remaining = remaining_until(deadline);
        let result = wait_for_prepare_ok(
            &mut transaction,
            repo_path,
            target_ref,
            expected_oid,
            remaining,
        )
        .await;

        match result {
            Ok(()) => return Ok(GitTargetRefHold { transaction }),
            Err(error) => {
                let _ = shutdown_update_ref_transaction(transaction, shutdown_timeout).await;

                if is_lock_contention_error(&error) {
                    let remaining = remaining_until(deadline);
                    if remaining.is_zero() {
                        return Err(TargetRefHoldError::Internal(
                            TARGET_REF_LOCK_TIMEOUT_MESSAGE.into(),
                        ));
                    }

                    sleep(HOLD_RETRY_INTERVAL.min(remaining)).await;
                    continue;
                }

                return Err(error);
            }
        }
    }
}

async fn spawn_update_ref_transaction(
    repo_path: &Path,
) -> Result<UpdateRefTransaction, TargetRefHoldError> {
    let mut child = Command::new("git")
        .args(["update-ref", "--stdin"])
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| TargetRefHoldError::Internal(err.to_string()))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| TargetRefHoldError::Internal("git update-ref missing stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| TargetRefHoldError::Internal("git update-ref missing stdout".into()))?;
    let stderr = child.stderr.take();

    Ok(UpdateRefTransaction {
        child,
        stdin: Some(stdin),
        stdout: BufReader::new(stdout),
        stderr,
    })
}

async fn write_update_ref_commands(
    stdin: &mut ChildStdin,
    commands: &str,
) -> Result<(), TargetRefHoldError> {
    stdin
        .write_all(commands.as_bytes())
        .await
        .map_err(|err| TargetRefHoldError::Internal(err.to_string()))?;
    stdin
        .flush()
        .await
        .map_err(|err| TargetRefHoldError::Internal(err.to_string()))
}

async fn wait_for_prepare_ok(
    transaction: &mut UpdateRefTransaction,
    repo_path: &Path,
    target_ref: &GitRef,
    expected_oid: &CommitOid,
    ready_timeout: Duration,
) -> Result<(), TargetRefHoldError> {
    let deadline = Instant::now() + ready_timeout;

    loop {
        let child = &mut transaction.child;
        let stdout = &mut transaction.stdout;
        let mut line = String::new();

        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(|err| TargetRefHoldError::Internal(err.to_string()))?;
                return classify_exited_transaction(transaction, status, repo_path, target_ref, expected_oid).await;
            }
            read = timeout_at(deadline, stdout.read_line(&mut line)) => {
                match read {
                    Err(_) => {
                        ensure_target_ref_matches(repo_path, target_ref, expected_oid).await?;
                        return Err(TargetRefHoldError::Internal(
                            TARGET_REF_LOCK_TIMEOUT_MESSAGE.into(),
                        ));
                    }
                    Ok(Err(err)) => {
                        return Err(TargetRefHoldError::Internal(err.to_string()));
                    }
                    Ok(Ok(0)) => {
                        let status = transaction
                            .child
                            .wait()
                            .await
                            .map_err(|err| TargetRefHoldError::Internal(err.to_string()))?;
                        return classify_exited_transaction(transaction, status, repo_path, target_ref, expected_oid).await;
                    }
                    Ok(Ok(_)) => {
                        match line.trim_end_matches(['\r', '\n']) {
                            "" | "start: ok" => {}
                            "prepare: ok" => return Ok(()),
                            other => {
                                return Err(TargetRefHoldError::Internal(format!(
                                    "unexpected git update-ref output: {other}"
                                )));
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn classify_exited_transaction(
    transaction: &mut UpdateRefTransaction,
    status: std::process::ExitStatus,
    repo_path: &Path,
    target_ref: &GitRef,
    expected_oid: &CommitOid,
) -> Result<(), TargetRefHoldError> {
    let stdout = drain_stdout(&mut transaction.stdout)
        .await
        .map_err(|err| TargetRefHoldError::Internal(err.to_string()))?;
    let stderr = drain_stderr(&mut transaction.stderr)
        .await
        .map_err(|err| TargetRefHoldError::Internal(err.to_string()))?;
    ensure_target_ref_matches(repo_path, target_ref, expected_oid).await?;

    Err(TargetRefHoldError::Internal(update_ref_failure_message(
        stdout.trim(),
        stderr.trim(),
        status,
    )))
}

async fn shutdown_update_ref_transaction(
    mut transaction: UpdateRefTransaction,
    shutdown_timeout: Duration,
) -> Result<(), String> {
    if let Some(mut stdin) = transaction.stdin.take() {
        if stdin.write_all(b"abort\n").await.is_ok() {
            let _ = stdin.flush().await;
        }
        drop(stdin);
    }

    let status = wait_for_exit(&mut transaction.child, shutdown_timeout).await?;

    let stdout = drain_stdout(&mut transaction.stdout)
        .await
        .map_err(|err| err.to_string())?;
    let stderr = drain_stderr(&mut transaction.stderr)
        .await
        .map_err(|err| err.to_string())?;

    if status.success() {
        return Ok(());
    }

    Err(update_ref_failure_message(
        stdout.trim(),
        stderr.trim(),
        status,
    ))
}

async fn drain_stdout(stdout: &mut BufReader<ChildStdout>) -> Result<String, std::io::Error> {
    let mut buffer = String::new();
    stdout.read_to_string(&mut buffer).await?;
    Ok(buffer)
}

async fn drain_stderr(stderr: &mut Option<ChildStderr>) -> Result<String, std::io::Error> {
    let mut buffer = vec![];
    if let Some(mut stderr_reader) = stderr.take() {
        stderr_reader.read_to_end(&mut buffer).await?;
    }
    Ok(String::from_utf8_lossy(&buffer).to_string())
}

fn stderr_message(stderr: &[u8]) -> String {
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    if message.is_empty() {
        "git update-ref failed".into()
    } else {
        message
    }
}

fn remaining_until(deadline: Instant) -> Duration {
    deadline
        .checked_duration_since(Instant::now())
        .unwrap_or(Duration::ZERO)
}

async fn ensure_target_ref_matches(
    repo_path: &Path,
    target_ref: &GitRef,
    expected_oid: &CommitOid,
) -> Result<(), TargetRefHoldError> {
    let resolved = resolve_ref_oid(repo_path, target_ref)
        .await
        .map_err(|err| TargetRefHoldError::Internal(err.to_string()))?;

    if resolved.as_ref().is_some_and(|oid| oid == expected_oid) {
        Ok(())
    } else {
        Err(TargetRefHoldError::Stale)
    }
}

async fn wait_for_exit(
    child: &mut Child,
    shutdown_timeout: Duration,
) -> Result<std::process::ExitStatus, String> {
    match timeout(shutdown_timeout, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => {
            child.kill().await.map_err(|err| err.to_string())?;
            match timeout(shutdown_timeout, child.wait()).await {
                Ok(Ok(status)) => Ok(status),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("timed out waiting for git update-ref to exit after kill".into()),
            }
        }
    }
}

fn update_ref_failure_message(
    stdout: &str,
    stderr: &str,
    status: std::process::ExitStatus,
) -> String {
    if !stderr.is_empty() {
        return stderr_message(stderr.as_bytes());
    }

    if !stdout.is_empty() {
        return stderr_message(stdout.as_bytes());
    }

    format!("git update-ref exited with status {status}")
}

fn is_lock_contention_error(error: &TargetRefHoldError) -> bool {
    matches!(error, TargetRefHoldError::Internal(message) if message.contains("cannot lock ref"))
}

fn map_git_port_error(error: GitCommandError) -> GitPortError {
    GitPortError::Internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command as StdCommand;

    use ingot_test_support::git::{
        git_output, run_git as git, temp_git_repo as make_temp_repo, write_file,
    };

    use super::*;

    fn temp_git_repo() -> PathBuf {
        make_temp_repo("ingot-git-refs")
    }

    #[tokio::test]
    async fn verify_and_hold_target_ref_rejects_concurrent_update_ref_until_released() {
        let repo = temp_git_repo();
        let port = GitJobCompletionPort;
        let current = CommitOid::new(git_output(&repo, &["rev-parse", "HEAD"]));
        let next = detached_commit(&repo, current.as_str());

        let hold = port
            .verify_and_hold_target_ref(&repo, &GitRef::new("refs/heads/main"), &current)
            .await
            .expect("acquire target ref hold");

        let output = StdCommand::new("git")
            .args(["update-ref", "refs/heads/main", &next, current.as_str()])
            .current_dir(&repo)
            .output()
            .expect("run concurrent update-ref");
        assert!(
            !output.status.success(),
            "concurrent update-ref should fail while hold is active"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("cannot lock ref"),
            "concurrent update-ref should fail because the ref is locked"
        );

        port.release_hold(hold).await.expect("release hold");

        let status = StdCommand::new("git")
            .args(["update-ref", "refs/heads/main", &next, current.as_str()])
            .current_dir(&repo)
            .status()
            .expect("rerun update-ref after release");
        assert!(status.success(), "update-ref should succeed after release");
        assert_eq!(git_output(&repo, &["rev-parse", "refs/heads/main"]), next);
    }

    #[tokio::test]
    async fn verify_and_hold_target_ref_rejects_stale_targets() {
        let repo = temp_git_repo();
        let port = GitJobCompletionPort;

        let result = port
            .verify_and_hold_target_ref(
                &repo,
                &GitRef::new("refs/heads/main"),
                &CommitOid::new("deadbeef"),
            )
            .await;

        assert!(matches!(result, Err(TargetRefHoldError::Stale)));
    }

    #[tokio::test]
    async fn verify_and_hold_target_ref_waits_for_its_own_prepare_ack() {
        let repo = temp_git_repo();
        let current = CommitOid::new(git_output(&repo, &["rev-parse", "HEAD"]));
        let target_ref = GitRef::new("refs/heads/main");
        let external_hold = verify_and_hold_target_ref_with_timeout(
            &repo,
            &target_ref,
            &current,
            HOLD_READY_TIMEOUT,
            HOLD_SHUTDOWN_TIMEOUT,
        )
        .await
        .expect("acquire external hold");

        let repo_for_task = repo.clone();
        let current_for_task = current.clone();
        let target_ref_for_task = target_ref.clone();
        let mut acquisition = tokio::spawn(async move {
            verify_and_hold_target_ref_with_timeout(
                &repo_for_task,
                &target_ref_for_task,
                &current_for_task,
                Duration::from_secs(1),
                HOLD_SHUTDOWN_TIMEOUT,
            )
            .await
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut acquisition)
                .await
                .is_err(),
            "acquisition should remain pending while another process holds the ref lock"
        );

        GitJobCompletionPort
            .release_hold(external_hold)
            .await
            .expect("release external hold");

        let hold = tokio::time::timeout(Duration::from_secs(1), acquisition)
            .await
            .expect("acquisition should finish after external hold release")
            .expect("join acquisition")
            .expect("acquire hold");
        GitJobCompletionPort
            .release_hold(hold)
            .await
            .expect("release acquired hold");
    }

    #[tokio::test]
    async fn verify_and_hold_target_ref_times_out_without_hanging_shutdown() {
        let repo = temp_git_repo();
        let current = CommitOid::new(git_output(&repo, &["rev-parse", "HEAD"]));
        let target_ref = GitRef::new("refs/heads/main");
        let external_hold = verify_and_hold_target_ref_with_timeout(
            &repo,
            &target_ref,
            &current,
            HOLD_READY_TIMEOUT,
            HOLD_SHUTDOWN_TIMEOUT,
        )
        .await
        .expect("acquire external hold");

        let started = Instant::now();
        let result = verify_and_hold_target_ref_with_timeout(
            &repo,
            &target_ref,
            &current,
            Duration::from_millis(100),
            Duration::from_millis(100),
        )
        .await;
        let elapsed = started.elapsed();

        assert!(matches!(
            result,
            Err(TargetRefHoldError::Internal(message))
                if message == "timed out waiting for target ref lock"
        ));
        assert!(
            elapsed < Duration::from_secs(1),
            "timeout path should return promptly instead of waiting indefinitely"
        );

        GitJobCompletionPort
            .release_hold(external_hold)
            .await
            .expect("release external hold");
    }

    fn detached_commit(path: &Path, parent: &str) -> String {
        write_file(&path.join("tracked.txt"), "next");
        git(path, &["add", "tracked.txt"]);
        let tree = git_output(path, &["write-tree"]);
        let output = StdCommand::new("git")
            .args(["commit-tree", tree.trim(), "-p", parent, "-m", "next"])
            .stdout(Stdio::piped())
            .current_dir(path)
            .spawn()
            .expect("spawn commit-tree")
            .wait_with_output()
            .expect("commit-tree output");
        assert!(output.status.success(), "commit-tree failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}
