use ingot_agent_adapters::registry::{bootstrap_codex_agent, probe_and_apply};
use ingot_store_sqlite::Database;
use tracing::info;

use crate::RuntimeError;

pub async fn ensure_default_agent(db: &Database) -> Result<(), RuntimeError> {
    ensure_default_agent_with(db, bootstrap_codex_agent()).await
}

async fn ensure_default_agent_with(
    db: &Database,
    mut agent: ingot_domain::agent::Agent,
) -> Result<(), RuntimeError> {
    if !db.list_agents().await?.is_empty() {
        return Ok(());
    }

    probe_and_apply(&mut agent).await;
    db.create_agent(&agent).await?;

    info!(
        agent_id = %agent.id,
        slug = %agent.slug,
        status = ?agent.status,
        model = %agent.model,
        "bootstrapped default agent during startup reconciliation"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ensure_default_agent_with;
    use std::fs;
    use std::path::{Path, PathBuf};

    use ingot_agent_adapters::registry::bootstrap_codex_agent_with;
    use ingot_domain::agent::AgentStatus;
    use ingot_test_support::git::unique_temp_path;
    use ingot_test_support::sqlite::migrated_test_db;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn ensure_default_agent_creates_bootstrap_agent_when_registry_is_empty() {
        let root = unique_temp_path("runtime-bootstrap");
        let db = migrated_test_db("runtime-bootstrap").await;
        let fake_codex = write_fake_codex_cli(&root);

        ensure_default_agent_with(
            &db,
            bootstrap_codex_agent_with(fake_codex.to_string_lossy(), "gpt-5.4"),
        )
        .await
        .expect("bootstrap default agent");

        let agents = db.list_agents().await.expect("list agents");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, AgentStatus::Available);
        assert_eq!(agents[0].model, "gpt-5.4");

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn ensure_default_agent_is_idempotent_when_agents_already_exist() {
        let root = unique_temp_path("runtime-bootstrap-idempotent");
        let db = migrated_test_db("runtime-bootstrap-idempotent").await;
        let fake_codex = write_fake_codex_cli(&root);
        let fake_codex_path = fake_codex.to_string_lossy().to_string();

        ensure_default_agent_with(&db, bootstrap_codex_agent_with(&fake_codex_path, "gpt-5.4"))
            .await
            .expect("first bootstrap");
        ensure_default_agent_with(&db, bootstrap_codex_agent_with(&fake_codex_path, "gpt-6"))
            .await
            .expect("second bootstrap");

        let agents = db.list_agents().await.expect("list agents");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].model, "gpt-5.4");

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn ensure_default_agent_persists_unavailable_agent_when_probe_fails() {
        let root = unique_temp_path("runtime-bootstrap-failed-probe");
        let db = migrated_test_db("runtime-bootstrap-failed-probe").await;
        let fake_codex = write_bad_codex_cli(&root);

        ensure_default_agent_with(
            &db,
            bootstrap_codex_agent_with(fake_codex.to_string_lossy(), "gpt-5.4"),
        )
        .await
        .expect("bootstrap unavailable default agent");

        let agents = db.list_agents().await.expect("list agents");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].status, AgentStatus::Unavailable);
        assert!(
            agents[0]
                .health_check
                .as_deref()
                .is_some_and(|message| message.contains("missing required flags"))
        );

        let _ = fs::remove_dir_all(root);
    }

    fn write_fake_codex_cli(root: &Path) -> PathBuf {
        fs::create_dir_all(root).expect("create test root");
        let path = root.join("fake-codex.sh");
        fs::write(
            &path,
            "#!/bin/sh\necho '--sandbox --output-schema --output-last-message --json'\n",
        )
        .expect("write fake codex");

        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&path)
                .expect("fake codex metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("chmod fake codex");
        }

        path
    }

    fn write_bad_codex_cli(root: &Path) -> PathBuf {
        fs::create_dir_all(root).expect("create test root");
        let path = root.join("fake-codex-bad.sh");
        fs::write(&path, "#!/bin/sh\necho '--json'\n").expect("write bad fake codex");

        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&path)
                .expect("bad fake codex metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("chmod bad fake codex");
        }

        path
    }
}
