use std::collections::HashSet;

use ingot_agent_adapters::registry::{
    bootstrap_claude_code_agent, bootstrap_codex_agent, probe_and_apply,
};
use ingot_domain::agent::Agent;
use ingot_store_sqlite::Database;
use tracing::info;

use crate::RuntimeError;

pub async fn ensure_default_agents(db: &Database) -> Result<(), RuntimeError> {
    ensure_default_agents_from(
        db,
        vec![bootstrap_codex_agent(), bootstrap_claude_code_agent()],
    )
    .await
}

async fn ensure_default_agents_from(
    db: &Database,
    candidates: Vec<Agent>,
) -> Result<(), RuntimeError> {
    let existing_slugs: HashSet<String> = db
        .list_agents()
        .await?
        .into_iter()
        .map(|agent| agent.slug)
        .collect();

    for mut agent in candidates {
        if existing_slugs.contains(&agent.slug) {
            continue;
        }

        probe_and_apply(&mut agent).await;
        db.create_agent(&agent).await?;

        info!(
            agent_id = %agent.id,
            slug = %agent.slug,
            status = ?agent.status,
            model = %agent.model,
            "bootstrapped agent during startup reconciliation"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ensure_default_agents_from;
    use std::fs;
    use std::path::{Path, PathBuf};

    use ingot_agent_adapters::registry::{
        bootstrap_claude_code_agent_with, bootstrap_codex_agent_with,
    };
    use ingot_domain::agent::AgentStatus;
    use ingot_test_support::env::temp_dir;
    use ingot_test_support::sqlite::migrated_test_db;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn creates_all_available_agents_when_registry_is_empty() {
        let root = temp_dir("runtime-bootstrap-both");
        let db = migrated_test_db("runtime-bootstrap-both").await;
        let fake_codex = write_fake_codex_cli(&root);
        let fake_claude = write_fake_claude_cli(&root);

        ensure_default_agents_from(
            &db,
            vec![
                bootstrap_codex_agent_with(fake_codex, "gpt-5.4"),
                bootstrap_claude_code_agent_with(fake_claude, "claude-sonnet-4-6"),
            ],
        )
        .await
        .expect("bootstrap agents");

        let agents = db.list_agents().await.expect("list agents");
        assert_eq!(agents.len(), 2);
        assert!(agents.iter().any(|a| a.slug == "codex"));
        assert!(agents.iter().any(|a| a.slug == "claude-code"));
        assert!(agents.iter().all(|a| a.status == AgentStatus::Available));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn skips_agents_whose_slug_already_exists() {
        let root = temp_dir("runtime-bootstrap-idempotent");
        let db = migrated_test_db("runtime-bootstrap-idempotent").await;
        let fake_codex = write_fake_codex_cli(&root);
        let fake_claude = write_fake_claude_cli(&root);

        ensure_default_agents_from(
            &db,
            vec![bootstrap_codex_agent_with(fake_codex.clone(), "gpt-5.4")],
        )
        .await
        .expect("first bootstrap");

        ensure_default_agents_from(
            &db,
            vec![
                bootstrap_codex_agent_with(fake_codex, "gpt-6"),
                bootstrap_claude_code_agent_with(fake_claude, "claude-sonnet-4-6"),
            ],
        )
        .await
        .expect("second bootstrap");

        let agents = db.list_agents().await.expect("list agents");
        assert_eq!(agents.len(), 2);

        let codex = agents.iter().find(|a| a.slug == "codex").unwrap();
        assert_eq!(codex.model, "gpt-5.4", "existing agent not overwritten");

        let claude = agents.iter().find(|a| a.slug == "claude-code").unwrap();
        assert_eq!(claude.model, "claude-sonnet-4-6");

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn creates_unavailable_agent_when_probe_fails() {
        let root = temp_dir("runtime-bootstrap-unavailable");
        let db = migrated_test_db("runtime-bootstrap-unavailable").await;
        let fake_codex = write_bad_codex_cli(&root);

        ensure_default_agents_from(&db, vec![bootstrap_codex_agent_with(fake_codex, "gpt-5.4")])
            .await
            .expect("bootstrap unavailable agent");

        let agents = db.list_agents().await.expect("list agents");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, AgentStatus::Unavailable);
        assert!(
            agents[0]
                .health_check
                .as_deref()
                .is_some_and(|msg| msg.contains("missing required flags"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn creates_both_agents_with_mixed_availability_when_one_probe_fails() {
        let root = temp_dir("runtime-bootstrap-mixed");
        let db = migrated_test_db("runtime-bootstrap-mixed").await;
        let bad_codex = write_bad_codex_cli(&root);
        let good_claude = write_fake_claude_cli(&root);

        ensure_default_agents_from(
            &db,
            vec![
                bootstrap_codex_agent_with(bad_codex, "gpt-5.4"),
                bootstrap_claude_code_agent_with(good_claude, "claude-sonnet-4-6"),
            ],
        )
        .await
        .expect("bootstrap mixed agents");

        let agents = db.list_agents().await.expect("list agents");
        assert_eq!(agents.len(), 2);

        let codex = agents.iter().find(|a| a.slug == "codex").unwrap();
        assert_eq!(codex.status, AgentStatus::Unavailable);

        let claude = agents.iter().find(|a| a.slug == "claude-code").unwrap();
        assert_eq!(claude.status, AgentStatus::Available);

        let _ = fs::remove_dir_all(root);
    }

    fn write_fake_codex_cli(root: &Path) -> PathBuf {
        write_script(
            root,
            "fake-codex.sh",
            "#!/bin/sh\necho '--config --sandbox --output-schema --output-last-message --json'\n",
        )
    }

    fn write_fake_claude_cli(root: &Path) -> PathBuf {
        write_script(
            root,
            "fake-claude.sh",
            "#!/bin/sh\necho '--print --output-format --json-schema --model'\n",
        )
    }

    fn write_bad_codex_cli(root: &Path) -> PathBuf {
        write_script(root, "fake-codex-bad.sh", "#!/bin/sh\necho '--json'\n")
    }

    fn write_script(root: &Path, name: &str, body: &str) -> PathBuf {
        fs::create_dir_all(root).expect("create test root");
        let path = root.join(name);
        fs::write(&path, body).expect("write script");

        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("chmod script");
        }

        path
    }
}
