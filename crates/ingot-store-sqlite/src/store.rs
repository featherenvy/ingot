use std::str::FromStr;

use chrono::Utc;
use ingot_domain::activity::Activity;
use ingot_domain::agent::Agent;
use ingot_domain::convergence::Convergence;
use ingot_domain::finding::Finding;
use ingot_domain::git_operation::GitOperation;
use ingot_domain::ids::{
    AgentId, FindingId, ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId,
};
use ingot_domain::item::{EscalationReason, EscalationState, Item};
use ingot_domain::job::{Job, JobStatus, OutcomeClass};
use ingot_domain::ports::{
    CompletedJobCompletion, JobCompletionContext, JobCompletionMutation, JobCompletionRepository,
    RepositoryError,
};
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_domain::revision_context::RevisionContext;
use ingot_domain::workspace::Workspace;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, Transaction};

use crate::db::Database;

#[derive(Debug, thiserror::Error)]
enum StoreDecodeError {
    #[error("invalid enum value {value:?}: {message}")]
    Enum { value: String, message: String },
    #[error("invalid json value: {0}")]
    Json(String),
    #[error("invalid id value {value:?}: {message}")]
    Id { value: String, message: String },
}

pub struct StartJobExecutionParams<'a> {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub workspace_id: Option<WorkspaceId>,
    pub agent_id: Option<AgentId>,
    pub lease_owner_id: &'a str,
    pub process_pid: Option<u32>,
    pub lease_expires_at: chrono::DateTime<Utc>,
}

pub struct FinishJobNonSuccessParams<'a> {
    pub job_id: JobId,
    pub item_id: ItemId,
    pub expected_item_revision_id: ItemRevisionId,
    pub status: JobStatus,
    pub outcome_class: Option<OutcomeClass>,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub escalation_reason: Option<EscalationReason>,
}

impl Database {
    pub async fn list_projects(&self) -> Result<Vec<Project>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT id, name, path, default_branch, color, created_at, updated_at
             FROM projects
             ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_project).collect()
    }

    pub async fn get_project(&self, project_id: ProjectId) -> Result<Project, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, name, path, default_branch, color, created_at, updated_at
             FROM projects
             WHERE id = ?",
        )
        .bind(project_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref()
            .map(map_project)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_project(&self, project: &Project) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO projects (id, name, path, default_branch, color, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(project.id.to_string())
        .bind(&project.name)
        .bind(&project.path)
        .bind(&project.default_branch)
        .bind(&project.color)
        .bind(project.created_at)
        .bind(project.updated_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_project(&self, project: &Project) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE projects
             SET name = ?, path = ?, default_branch = ?, color = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(&project.name)
        .bind(&project.path)
        .bind(&project.default_branch)
        .bind(&project.color)
        .bind(project.updated_at)
        .bind(project.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn delete_project(&self, project_id: ProjectId) -> Result<(), RepositoryError> {
        let result = sqlx::query("DELETE FROM projects WHERE id = ?")
            .bind(project_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn list_agents(&self) -> Result<Vec<Agent>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT id, slug, name, adapter_kind, provider, model, cli_path, capabilities,
                    health_check, status
             FROM agents
             ORDER BY slug ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_agent).collect()
    }

    pub async fn get_agent(&self, agent_id: AgentId) -> Result<Agent, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, slug, name, adapter_kind, provider, model, cli_path, capabilities,
                    health_check, status
             FROM agents
             WHERE id = ?",
        )
        .bind(agent_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref()
            .map(map_agent)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_agent(&self, agent: &Agent) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO agents (
                id, slug, name, adapter_kind, provider, model, cli_path, capabilities,
                health_check, status
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(agent.id.to_string())
        .bind(&agent.slug)
        .bind(&agent.name)
        .bind(encode_enum(&agent.adapter_kind)?)
        .bind(&agent.provider)
        .bind(&agent.model)
        .bind(&agent.cli_path)
        .bind(serde_json::to_string(&agent.capabilities).map_err(json_err)?)
        .bind(agent.health_check.as_deref())
        .bind(encode_enum(&agent.status)?)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_agent(&self, agent: &Agent) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE agents
             SET slug = ?, name = ?, adapter_kind = ?, provider = ?, model = ?, cli_path = ?,
                 capabilities = ?, health_check = ?, status = ?
             WHERE id = ?",
        )
        .bind(&agent.slug)
        .bind(&agent.name)
        .bind(encode_enum(&agent.adapter_kind)?)
        .bind(&agent.provider)
        .bind(&agent.model)
        .bind(&agent.cli_path)
        .bind(serde_json::to_string(&agent.capabilities).map_err(json_err)?)
        .bind(agent.health_check.as_deref())
        .bind(encode_enum(&agent.status)?)
        .bind(agent.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn delete_agent(&self, agent_id: AgentId) -> Result<(), RepositoryError> {
        let result = sqlx::query("DELETE FROM agents WHERE id = ?")
            .bind(agent_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn create_git_operation(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO git_operations (
                id, project_id, operation_kind, entity_type, entity_id, workspace_id, ref_name,
                expected_old_oid, new_oid, commit_oid, status, metadata, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(operation.id.to_string())
        .bind(operation.project_id.to_string())
        .bind(encode_enum(&operation.operation_kind)?)
        .bind(encode_enum(&operation.entity_type)?)
        .bind(&operation.entity_id)
        .bind(operation.workspace_id.map(|id| id.to_string()))
        .bind(operation.ref_name.as_deref())
        .bind(operation.expected_old_oid.as_deref())
        .bind(operation.new_oid.as_deref())
        .bind(operation.commit_oid.as_deref())
        .bind(encode_enum(&operation.status)?)
        .bind(
            operation
                .metadata
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(json_err)?,
        )
        .bind(operation.created_at)
        .bind(operation.completed_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn append_activity(&self, activity: &Activity) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO activity (
                id, project_id, event_type, entity_type, entity_id, payload, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(activity.id.to_string())
        .bind(activity.project_id.to_string())
        .bind(encode_enum(&activity.event_type)?)
        .bind(&activity.entity_type)
        .bind(&activity.entity_id)
        .bind(serde_json::to_string(&activity.payload).map_err(json_err)?)
        .bind(activity.created_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn list_activity_by_project(
        &self,
        project_id: ProjectId,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Activity>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT id, project_id, event_type, entity_type, entity_id, payload, created_at
             FROM activity
             WHERE project_id = ?
             ORDER BY created_at DESC
             LIMIT ? OFFSET ?",
        )
        .bind(project_id.to_string())
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_activity).collect()
    }

    pub async fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE git_operations
             SET workspace_id = ?, ref_name = ?, expected_old_oid = ?, new_oid = ?, commit_oid = ?,
                 status = ?, metadata = ?, completed_at = ?
             WHERE id = ?",
        )
        .bind(operation.workspace_id.map(|id| id.to_string()))
        .bind(operation.ref_name.as_deref())
        .bind(operation.expected_old_oid.as_deref())
        .bind(operation.new_oid.as_deref())
        .bind(operation.commit_oid.as_deref())
        .bind(encode_enum(&operation.status)?)
        .bind(
            operation
                .metadata
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(json_err)?,
        )
        .bind(operation.completed_at)
        .bind(operation.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn list_unresolved_git_operations(
        &self,
    ) -> Result<Vec<GitOperation>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT id, project_id, operation_kind, entity_type, entity_id, workspace_id, ref_name,
                    expected_old_oid, new_oid, commit_oid, status, metadata, created_at, completed_at
             FROM git_operations
             WHERE status IN ('planned', 'applied')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_git_operation).collect()
    }

    pub async fn list_items_by_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<Item>, RepositoryError> {
        let rows = sqlx::query("SELECT * FROM items WHERE project_id = ? ORDER BY created_at DESC")
            .bind(project_id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;

        rows.iter().map(map_item).collect()
    }

    pub async fn get_item(&self, item_id: ItemId) -> Result<Item, RepositoryError> {
        let row = sqlx::query("SELECT * FROM items WHERE id = ?")
            .bind(item_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_item)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn update_item(&self, item: &Item) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE items
             SET classification = ?, workflow_version = ?, lifecycle_state = ?, parking_state = ?,
                 done_reason = ?, resolution_source = ?, approval_state = ?, escalation_state = ?,
                 escalation_reason = ?, current_revision_id = ?, origin_kind = ?, origin_finding_id = ?,
                 priority = ?, labels = ?, operator_notes = ?, updated_at = ?, closed_at = ?
             WHERE id = ?",
        )
        .bind(encode_enum(&item.classification)?)
        .bind(&item.workflow_version)
        .bind(encode_enum(&item.lifecycle_state)?)
        .bind(encode_enum(&item.parking_state)?)
        .bind(item.done_reason.as_ref().map(encode_enum).transpose()?)
        .bind(item.resolution_source.as_ref().map(encode_enum).transpose()?)
        .bind(encode_enum(&item.approval_state)?)
        .bind(encode_enum(&item.escalation_state)?)
        .bind(item.escalation_reason.as_ref().map(encode_enum).transpose()?)
        .bind(item.current_revision_id.to_string())
        .bind(encode_enum(&item.origin_kind)?)
        .bind(item.origin_finding_id.map(|id| id.to_string()))
        .bind(encode_enum(&item.priority)?)
        .bind(serde_json::to_string(&item.labels).map_err(json_err)?)
        .bind(item.operator_notes.as_deref())
        .bind(item.updated_at)
        .bind(item.closed_at)
        .bind(item.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn list_revisions_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<ItemRevision>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM item_revisions WHERE item_id = ? ORDER BY revision_no DESC")
                .bind(item_id.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;

        rows.iter().map(map_revision).collect()
    }

    pub async fn get_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<ItemRevision, RepositoryError> {
        let row = sqlx::query("SELECT * FROM item_revisions WHERE id = ?")
            .bind(revision_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_revision)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_revision(&self, revision: &ItemRevision) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(revision.id.to_string())
        .bind(revision.item_id.to_string())
        .bind(revision.revision_no as i64)
        .bind(&revision.title)
        .bind(&revision.description)
        .bind(&revision.acceptance_criteria)
        .bind(&revision.target_ref)
        .bind(encode_enum(&revision.approval_policy)?)
        .bind(serde_json::to_string(&revision.policy_snapshot).map_err(json_err)?)
        .bind(serde_json::to_string(&revision.template_map_snapshot).map_err(json_err)?)
        .bind(&revision.seed_commit_oid)
        .bind(revision.seed_target_commit_oid.as_deref())
        .bind(revision.supersedes_revision_id.map(|id| id.to_string()))
        .bind(revision.created_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn create_item_with_revision(
        &self,
        item: &Item,
        revision: &ItemRevision,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                done_reason, resolution_source, approval_state, escalation_state, escalation_reason,
                current_revision_id, origin_kind, origin_finding_id, priority, labels, operator_notes,
                created_at, updated_at, closed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(item.id.to_string())
        .bind(item.project_id.to_string())
        .bind(encode_enum(&item.classification)?)
        .bind(&item.workflow_version)
        .bind(encode_enum(&item.lifecycle_state)?)
        .bind(encode_enum(&item.parking_state)?)
        .bind(item.done_reason.as_ref().map(encode_enum).transpose()?)
        .bind(item.resolution_source.as_ref().map(encode_enum).transpose()?)
        .bind(encode_enum(&item.approval_state)?)
        .bind(encode_enum(&item.escalation_state)?)
        .bind(item.escalation_reason.as_ref().map(encode_enum).transpose()?)
        .bind(item.current_revision_id.to_string())
        .bind(encode_enum(&item.origin_kind)?)
        .bind(item.origin_finding_id.map(|id| id.to_string()))
        .bind(encode_enum(&item.priority)?)
        .bind(serde_json::to_string(&item.labels).map_err(json_err)?)
        .bind(item.operator_notes.as_deref())
        .bind(item.created_at)
        .bind(item.updated_at)
        .bind(item.closed_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(revision.id.to_string())
        .bind(revision.item_id.to_string())
        .bind(revision.revision_no as i64)
        .bind(&revision.title)
        .bind(&revision.description)
        .bind(&revision.acceptance_criteria)
        .bind(&revision.target_ref)
        .bind(encode_enum(&revision.approval_policy)?)
        .bind(serde_json::to_string(&revision.policy_snapshot).map_err(json_err)?)
        .bind(serde_json::to_string(&revision.template_map_snapshot).map_err(json_err)?)
        .bind(&revision.seed_commit_oid)
        .bind(revision.seed_target_commit_oid.as_deref())
        .bind(revision.supersedes_revision_id.map(|id| id.to_string()))
        .bind(revision.created_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub async fn get_revision_context(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<RevisionContext>, RepositoryError> {
        let row = sqlx::query("SELECT * FROM revision_contexts WHERE item_revision_id = ?")
            .bind(revision_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref().map(map_revision_context).transpose()
    }

    pub async fn upsert_revision_context(
        &self,
        context: &RevisionContext,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO revision_contexts (
                item_revision_id, schema_version, payload, updated_from_job_id, updated_at
             ) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(item_revision_id) DO UPDATE SET
                schema_version = excluded.schema_version,
                payload = excluded.payload,
                updated_from_job_id = excluded.updated_from_job_id,
                updated_at = excluded.updated_at",
        )
        .bind(context.item_revision_id.to_string())
        .bind(&context.schema_version)
        .bind(serde_json::to_string(&context.payload).map_err(json_err)?)
        .bind(context.updated_from_job_id.map(|id| id.to_string()))
        .bind(context.updated_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn list_jobs_by_item(&self, item_id: ItemId) -> Result<Vec<Job>, RepositoryError> {
        let rows = sqlx::query("SELECT * FROM jobs WHERE item_id = ? ORDER BY created_at DESC")
            .bind(item_id.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;

        rows.iter().map(map_job).collect()
    }

    pub async fn list_queued_jobs(&self, limit: u32) -> Result<Vec<Job>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM jobs
             WHERE status = 'queued'
             ORDER BY created_at ASC
             LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_job).collect()
    }

    pub async fn list_jobs_by_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<Job>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM jobs
             WHERE project_id = ?
             ORDER BY created_at DESC",
        )
        .bind(project_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_job).collect()
    }

    pub async fn list_active_jobs(&self) -> Result<Vec<Job>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM jobs
             WHERE status IN ('queued', 'assigned', 'running')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_job).collect()
    }

    pub async fn start_job_execution(
        &self,
        params: StartJobExecutionParams<'_>,
    ) -> Result<(), RepositoryError> {
        let StartJobExecutionParams {
            job_id,
            item_id,
            expected_item_revision_id,
            workspace_id,
            agent_id,
            lease_owner_id,
            process_pid,
            lease_expires_at,
        } = params;
        let result = sqlx::query(
            "UPDATE jobs
             SET status = 'running',
                 workspace_id = COALESCE(?, workspace_id),
                 agent_id = COALESCE(?, agent_id),
                 process_pid = ?,
                 lease_owner_id = ?,
                 heartbeat_at = ?,
                 lease_expires_at = ?,
                 started_at = COALESCE(started_at, ?)
             WHERE id = ?
               AND status IN ('queued', 'assigned')
               AND EXISTS (
                   SELECT 1
                   FROM items
                   WHERE id = ?
                     AND current_revision_id = ?
               )",
        )
        .bind(workspace_id.map(|id| id.to_string()))
        .bind(agent_id.map(|id| id.to_string()))
        .bind(process_pid.map(i64::from))
        .bind(lease_owner_id)
        .bind(Utc::now())
        .bind(lease_expires_at)
        .bind(Utc::now())
        .bind(job_id.to_string())
        .bind(item_id.to_string())
        .bind(expected_item_revision_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            return Err(classify_running_job_conflict(
                &self.pool,
                job_id,
                item_id,
                expected_item_revision_id,
                &["queued", "assigned"],
            )
            .await?);
        }

        Ok(())
    }

    pub async fn heartbeat_job_execution(
        &self,
        job_id: JobId,
        item_id: ItemId,
        expected_item_revision_id: ItemRevisionId,
        lease_owner_id: &str,
        lease_expires_at: chrono::DateTime<Utc>,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE jobs
             SET heartbeat_at = ?, lease_expires_at = ?
             WHERE id = ?
               AND status = 'running'
               AND lease_owner_id = ?
               AND EXISTS (
                   SELECT 1
                   FROM items
                   WHERE id = ?
                     AND current_revision_id = ?
               )",
        )
        .bind(Utc::now())
        .bind(lease_expires_at)
        .bind(job_id.to_string())
        .bind(lease_owner_id)
        .bind(item_id.to_string())
        .bind(expected_item_revision_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            return Err(classify_running_job_conflict(
                &self.pool,
                job_id,
                item_id,
                expected_item_revision_id,
                &["running"],
            )
            .await?);
        }

        Ok(())
    }

    pub async fn get_job(&self, job_id: JobId) -> Result<Job, RepositoryError> {
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ?")
            .bind(job_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_job)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_job(&self, job: &Job) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO jobs (
                id, project_id, item_id, item_revision_id, step_id, semantic_attempt_no, retry_no,
                supersedes_job_id, status, outcome_class, phase_kind, workspace_id, workspace_kind,
                execution_permission, context_policy, phase_template_slug, phase_template_digest,
                prompt_snapshot, input_base_commit_oid, input_head_commit_oid, output_artifact_kind,
                output_commit_oid, result_schema_version, result_payload, agent_id, process_pid,
                lease_owner_id, heartbeat_at, lease_expires_at, error_code, error_message,
                created_at, started_at, ended_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(job.id.to_string())
        .bind(job.project_id.to_string())
        .bind(job.item_id.to_string())
        .bind(job.item_revision_id.to_string())
        .bind(&job.step_id)
        .bind(job.semantic_attempt_no as i64)
        .bind(job.retry_no as i64)
        .bind(job.supersedes_job_id.map(|id| id.to_string()))
        .bind(encode_enum(&job.status)?)
        .bind(job.outcome_class.as_ref().map(encode_enum).transpose()?)
        .bind(encode_enum(&job.phase_kind)?)
        .bind(job.workspace_id.map(|id| id.to_string()))
        .bind(encode_enum(&job.workspace_kind)?)
        .bind(encode_enum(&job.execution_permission)?)
        .bind(encode_enum(&job.context_policy)?)
        .bind(&job.phase_template_slug)
        .bind(job.phase_template_digest.as_deref())
        .bind(job.prompt_snapshot.as_deref())
        .bind(job.input_base_commit_oid.as_deref())
        .bind(job.input_head_commit_oid.as_deref())
        .bind(encode_enum(&job.output_artifact_kind)?)
        .bind(job.output_commit_oid.as_deref())
        .bind(job.result_schema_version.as_deref())
        .bind(serialize_optional_json(job.result_payload.as_ref())?)
        .bind(job.agent_id.map(|id| id.to_string()))
        .bind(job.process_pid.map(i64::from))
        .bind(job.lease_owner_id.as_deref())
        .bind(job.heartbeat_at)
        .bind(job.lease_expires_at)
        .bind(job.error_code.as_deref())
        .bind(job.error_message.as_deref())
        .bind(job.created_at)
        .bind(job.started_at)
        .bind(job.ended_at)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        Ok(())
    }

    pub async fn update_job(&self, job: &Job) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE jobs
             SET step_id = ?, semantic_attempt_no = ?, retry_no = ?, supersedes_job_id = ?, status = ?,
                 outcome_class = ?, phase_kind = ?, workspace_id = ?, workspace_kind = ?,
                 execution_permission = ?, context_policy = ?, phase_template_slug = ?,
                 phase_template_digest = ?, prompt_snapshot = ?, input_base_commit_oid = ?,
                 input_head_commit_oid = ?, output_artifact_kind = ?, output_commit_oid = ?,
                 result_schema_version = ?, result_payload = ?, agent_id = ?, process_pid = ?,
                 lease_owner_id = ?, heartbeat_at = ?, lease_expires_at = ?, error_code = ?,
                 error_message = ?, created_at = ?, started_at = ?, ended_at = ?
             WHERE id = ?",
        )
        .bind(&job.step_id)
        .bind(job.semantic_attempt_no as i64)
        .bind(job.retry_no as i64)
        .bind(job.supersedes_job_id.map(|id| id.to_string()))
        .bind(encode_enum(&job.status)?)
        .bind(job.outcome_class.as_ref().map(encode_enum).transpose()?)
        .bind(encode_enum(&job.phase_kind)?)
        .bind(job.workspace_id.map(|id| id.to_string()))
        .bind(encode_enum(&job.workspace_kind)?)
        .bind(encode_enum(&job.execution_permission)?)
        .bind(encode_enum(&job.context_policy)?)
        .bind(&job.phase_template_slug)
        .bind(job.phase_template_digest.as_deref())
        .bind(job.prompt_snapshot.as_deref())
        .bind(job.input_base_commit_oid.as_deref())
        .bind(job.input_head_commit_oid.as_deref())
        .bind(encode_enum(&job.output_artifact_kind)?)
        .bind(job.output_commit_oid.as_deref())
        .bind(job.result_schema_version.as_deref())
        .bind(serialize_optional_json(job.result_payload.as_ref())?)
        .bind(job.agent_id.map(|id| id.to_string()))
        .bind(job.process_pid.map(i64::from))
        .bind(job.lease_owner_id.as_deref())
        .bind(job.heartbeat_at)
        .bind(job.lease_expires_at)
        .bind(job.error_code.as_deref())
        .bind(job.error_message.as_deref())
        .bind(job.created_at)
        .bind(job.started_at)
        .bind(job.ended_at)
        .bind(job.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn get_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Workspace, RepositoryError> {
        let row = sqlx::query("SELECT * FROM workspaces WHERE id = ?")
            .bind(workspace_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_workspace)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_workspace(&self, workspace: &Workspace) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO workspaces (
                id, project_id, kind, strategy, path, created_for_revision_id, parent_workspace_id,
                target_ref, workspace_ref, base_commit_oid, head_commit_oid, retention_policy,
                status, current_job_id, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(workspace.id.to_string())
        .bind(workspace.project_id.to_string())
        .bind(encode_enum(&workspace.kind)?)
        .bind(encode_enum(&workspace.strategy)?)
        .bind(&workspace.path)
        .bind(workspace.created_for_revision_id.map(|id| id.to_string()))
        .bind(workspace.parent_workspace_id.map(|id| id.to_string()))
        .bind(workspace.target_ref.as_deref())
        .bind(workspace.workspace_ref.as_deref())
        .bind(workspace.base_commit_oid.as_deref())
        .bind(workspace.head_commit_oid.as_deref())
        .bind(encode_enum(&workspace.retention_policy)?)
        .bind(encode_enum(&workspace.status)?)
        .bind(workspace.current_job_id.map(|id| id.to_string()))
        .bind(workspace.created_at)
        .bind(workspace.updated_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_workspace(&self, workspace: &Workspace) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE workspaces
             SET path = ?, target_ref = ?, workspace_ref = ?, base_commit_oid = ?, head_commit_oid = ?,
                 retention_policy = ?, status = ?, current_job_id = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(&workspace.path)
        .bind(workspace.target_ref.as_deref())
        .bind(workspace.workspace_ref.as_deref())
        .bind(workspace.base_commit_oid.as_deref())
        .bind(workspace.head_commit_oid.as_deref())
        .bind(encode_enum(&workspace.retention_policy)?)
        .bind(encode_enum(&workspace.status)?)
        .bind(workspace.current_job_id.map(|id| id.to_string()))
        .bind(workspace.updated_at)
        .bind(workspace.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn find_authoring_workspace_for_revision(
        &self,
        revision_id: ItemRevisionId,
    ) -> Result<Option<Workspace>, RepositoryError> {
        let row = sqlx::query(
            "SELECT *
             FROM workspaces
             WHERE created_for_revision_id = ?
               AND kind = 'authoring'
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(revision_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_workspace).transpose()
    }

    pub async fn list_workspaces_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<Workspace>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT w.*
             FROM workspaces w
             JOIN item_revisions r ON r.id = w.created_for_revision_id
             WHERE r.item_id = ?
             ORDER BY w.created_at DESC",
        )
        .bind(item_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_workspace).collect()
    }

    pub async fn list_workspaces_by_project(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<Workspace>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM workspaces
             WHERE project_id = ?
             ORDER BY created_at DESC",
        )
        .bind(project_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_workspace).collect()
    }

    pub async fn list_convergences_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<Convergence>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM convergences WHERE item_id = ? ORDER BY created_at DESC")
                .bind(item_id.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;

        rows.iter().map(map_convergence).collect()
    }

    pub async fn get_convergence(
        &self,
        convergence_id: ingot_domain::ids::ConvergenceId,
    ) -> Result<Convergence, RepositoryError> {
        let row = sqlx::query("SELECT * FROM convergences WHERE id = ?")
            .bind(convergence_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_convergence)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn list_active_convergences(&self) -> Result<Vec<Convergence>, RepositoryError> {
        let rows = sqlx::query(
            "SELECT *
             FROM convergences
             WHERE status IN ('queued', 'running')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;

        rows.iter().map(map_convergence).collect()
    }

    pub async fn create_convergence(
        &self,
        convergence: &Convergence,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO convergences (
                id, project_id, item_id, item_revision_id, source_workspace_id, integration_workspace_id,
                source_head_commit_oid, target_ref, strategy, status, input_target_commit_oid,
                prepared_commit_oid, final_target_commit_oid, conflict_summary, created_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(convergence.id.to_string())
        .bind(convergence.project_id.to_string())
        .bind(convergence.item_id.to_string())
        .bind(convergence.item_revision_id.to_string())
        .bind(convergence.source_workspace_id.to_string())
        .bind(convergence.integration_workspace_id.map(|id| id.to_string()))
        .bind(&convergence.source_head_commit_oid)
        .bind(&convergence.target_ref)
        .bind(encode_enum(&convergence.strategy)?)
        .bind(encode_enum(&convergence.status)?)
        .bind(convergence.input_target_commit_oid.as_deref())
        .bind(convergence.prepared_commit_oid.as_deref())
        .bind(convergence.final_target_commit_oid.as_deref())
        .bind(convergence.conflict_summary.as_deref())
        .bind(convergence.created_at)
        .bind(convergence.completed_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn update_convergence(
        &self,
        convergence: &Convergence,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            "UPDATE convergences
             SET integration_workspace_id = ?, source_head_commit_oid = ?, target_ref = ?, strategy = ?,
                 status = ?, input_target_commit_oid = ?, prepared_commit_oid = ?, final_target_commit_oid = ?,
                 conflict_summary = ?, completed_at = ?
             WHERE id = ?",
        )
        .bind(convergence.integration_workspace_id.map(|id| id.to_string()))
        .bind(&convergence.source_head_commit_oid)
        .bind(&convergence.target_ref)
        .bind(encode_enum(&convergence.strategy)?)
        .bind(encode_enum(&convergence.status)?)
        .bind(convergence.input_target_commit_oid.as_deref())
        .bind(convergence.prepared_commit_oid.as_deref())
        .bind(convergence.final_target_commit_oid.as_deref())
        .bind(convergence.conflict_summary.as_deref())
        .bind(convergence.completed_at)
        .bind(convergence.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound);
        }

        Ok(())
    }

    pub async fn list_findings_by_item(
        &self,
        item_id: ItemId,
    ) -> Result<Vec<Finding>, RepositoryError> {
        let rows =
            sqlx::query("SELECT * FROM findings WHERE source_item_id = ? ORDER BY created_at DESC")
                .bind(item_id.to_string())
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;

        rows.iter().map(map_finding).collect()
    }

    pub async fn get_finding(&self, finding_id: FindingId) -> Result<Finding, RepositoryError> {
        let row = sqlx::query("SELECT * FROM findings WHERE id = ?")
            .bind(finding_id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;

        row.as_ref()
            .map(map_finding)
            .transpose()?
            .ok_or(RepositoryError::NotFound)
    }

    pub async fn create_finding(&self, finding: &Finding) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO findings (
                id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
                source_report_schema_version, source_finding_key, source_subject_kind,
                source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
                paths, evidence, triage_state, promoted_item_id, dismissal_reason, created_at, triaged_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(finding.id.to_string())
        .bind(finding.project_id.to_string())
        .bind(finding.source_item_id.to_string())
        .bind(finding.source_item_revision_id.to_string())
        .bind(finding.source_job_id.to_string())
        .bind(&finding.source_step_id)
        .bind(&finding.source_report_schema_version)
        .bind(&finding.source_finding_key)
        .bind(encode_enum(&finding.source_subject_kind)?)
        .bind(finding.source_subject_base_commit_oid.as_deref())
        .bind(&finding.source_subject_head_commit_oid)
        .bind(&finding.code)
        .bind(encode_enum(&finding.severity)?)
        .bind(&finding.summary)
        .bind(serde_json::to_string(&finding.paths).map_err(json_err)?)
        .bind(serde_json::to_string(&finding.evidence).map_err(json_err)?)
        .bind(encode_enum(&finding.triage_state)?)
        .bind(finding.promoted_item_id.map(|id| id.to_string()))
        .bind(finding.dismissal_reason.as_deref())
        .bind(finding.created_at)
        .bind(finding.triaged_at)
        .execute(&self.pool)
        .await
        .map_err(db_write_err)?;

        Ok(())
    }

    pub async fn find_finding_by_source(
        &self,
        job_id: JobId,
        source_finding_key: &str,
    ) -> Result<Option<Finding>, RepositoryError> {
        let row = sqlx::query(
            "SELECT * FROM findings WHERE source_job_id = ? AND source_finding_key = ?",
        )
        .bind(job_id.to_string())
        .bind(source_finding_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;

        row.as_ref().map(map_finding).transpose()
    }

    pub async fn load_job_completion_context(
        &self,
        job_id: JobId,
    ) -> Result<JobCompletionContext, RepositoryError> {
        let job = self.get_job(job_id).await?;
        let item = self.get_item(job.item_id).await?;
        let project = self.get_project(item.project_id).await?;
        let revision = self.get_revision(item.current_revision_id).await?;
        let convergences = self.list_convergences_by_item(item.id).await?;

        Ok(JobCompletionContext {
            job,
            item,
            project,
            revision,
            convergences,
        })
    }

    pub async fn load_completed_job_completion(
        &self,
        job_id: JobId,
    ) -> Result<Option<CompletedJobCompletion>, RepositoryError> {
        let job = self.get_job(job_id).await?;
        if job.status != JobStatus::Completed {
            return Ok(None);
        }

        let finding_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM findings WHERE source_job_id = ?")
                .bind(job_id.to_string())
                .fetch_one(&self.pool)
                .await
                .map_err(db_err)?;

        Ok(Some(CompletedJobCompletion {
            job,
            finding_count: finding_count
                .try_into()
                .expect("finding count should fit into usize"),
        }))
    }

    pub async fn apply_job_completion(
        &self,
        mutation: JobCompletionMutation,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        let serialized_result_payload = serialize_optional_json(mutation.result_payload.as_ref())?;

        let result = if let Some(prepared_convergence_guard) =
            mutation.prepared_convergence_guard.as_ref()
        {
            sqlx::query(
                "UPDATE jobs
                 SET status = 'completed',
                     outcome_class = ?,
                     result_schema_version = ?,
                     result_payload = ?,
                     output_commit_oid = ?,
                     ended_at = ?
                 WHERE id = ?
                   AND status IN ('queued', 'assigned', 'running')
                   AND EXISTS (
                       SELECT 1
                       FROM items
                       WHERE id = ?
                         AND current_revision_id = ?
                   )
                   AND EXISTS (
                       SELECT 1
                       FROM convergences
                       WHERE id = ?
                         AND item_revision_id = ?
                         AND status = 'prepared'
                         AND target_ref = ?
                         AND input_target_commit_oid = ?
                   )",
            )
            .bind(encode_enum(&mutation.outcome_class)?)
            .bind(mutation.result_schema_version.as_deref())
            .bind(&serialized_result_payload)
            .bind(mutation.output_commit_oid.as_deref())
            .bind(Utc::now())
            .bind(mutation.job_id.to_string())
            .bind(mutation.item_id.to_string())
            .bind(mutation.expected_item_revision_id.to_string())
            .bind(prepared_convergence_guard.convergence_id.to_string())
            .bind(prepared_convergence_guard.item_revision_id.to_string())
            .bind(&prepared_convergence_guard.target_ref)
            .bind(&prepared_convergence_guard.expected_target_head_oid)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
        } else {
            sqlx::query(
                "UPDATE jobs
                 SET status = 'completed',
                     outcome_class = ?,
                     result_schema_version = ?,
                     result_payload = ?,
                     output_commit_oid = ?,
                     ended_at = ?
                 WHERE id = ?
                   AND status IN ('queued', 'assigned', 'running')
                   AND EXISTS (
                       SELECT 1
                       FROM items
                       WHERE id = ?
                         AND current_revision_id = ?
                   )",
            )
            .bind(encode_enum(&mutation.outcome_class)?)
            .bind(mutation.result_schema_version.as_deref())
            .bind(&serialized_result_payload)
            .bind(mutation.output_commit_oid.as_deref())
            .bind(Utc::now())
            .bind(mutation.job_id.to_string())
            .bind(mutation.item_id.to_string())
            .bind(mutation.expected_item_revision_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?
        };

        if result.rows_affected() != 1 {
            return Err(classify_job_completion_conflict(&mut tx, &mutation).await?);
        }

        for finding in &mutation.findings {
            upsert_finding(&mut tx, finding).await?;
        }

        if mutation.clear_item_escalation {
            let escalation = sqlx::query(
                "UPDATE items
                 SET escalation_state = ?, escalation_reason = NULL, updated_at = ?
                 WHERE id = ?
                   AND current_revision_id = ?",
            )
            .bind(encode_enum(&EscalationState::None)?)
            .bind(Utc::now())
            .bind(mutation.item_id.to_string())
            .bind(mutation.expected_item_revision_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

            if escalation.rows_affected() != 1 {
                return Err(RepositoryError::Conflict("job_revision_stale".into()));
            }
        }

        if let Some(prepared_convergence_guard) = mutation.prepared_convergence_guard.as_ref() {
            if let Some(approval_state) = prepared_convergence_guard.next_approval_state.as_ref() {
                let approval = sqlx::query(
                    "UPDATE items
                     SET approval_state = ?, updated_at = ?
                     WHERE id = ?
                       AND current_revision_id = ?",
                )
                .bind(encode_enum(approval_state)?)
                .bind(Utc::now())
                .bind(mutation.item_id.to_string())
                .bind(mutation.expected_item_revision_id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;

                if approval.rows_affected() != 1 {
                    return Err(RepositoryError::Conflict("job_revision_stale".into()));
                }
            }
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    pub async fn finish_job_non_success(
        &self,
        params: FinishJobNonSuccessParams<'_>,
    ) -> Result<(), RepositoryError> {
        let FinishJobNonSuccessParams {
            job_id,
            item_id,
            expected_item_revision_id,
            status,
            outcome_class,
            error_code,
            error_message,
            escalation_reason,
        } = params;
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        let result = sqlx::query(
            "UPDATE jobs
             SET status = ?,
                 outcome_class = ?,
                 result_schema_version = NULL,
                 result_payload = NULL,
                 output_commit_oid = NULL,
                 error_code = ?,
                 error_message = ?,
                 ended_at = ?
             WHERE id = ?
               AND status IN ('queued', 'assigned', 'running')
               AND EXISTS (
                   SELECT 1
                   FROM items
                   WHERE id = ?
                     AND current_revision_id = ?
               )",
        )
        .bind(encode_enum(&status)?)
        .bind(outcome_class.as_ref().map(encode_enum).transpose()?)
        .bind(error_code)
        .bind(error_message)
        .bind(Utc::now())
        .bind(job_id.to_string())
        .bind(item_id.to_string())
        .bind(expected_item_revision_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        if result.rows_affected() != 1 {
            return Err(classify_terminal_job_conflict(
                &mut tx,
                job_id,
                item_id,
                expected_item_revision_id,
            )
            .await?);
        }

        if let Some(escalation_reason) = escalation_reason {
            let escalation = sqlx::query(
                "UPDATE items
                 SET escalation_state = ?, escalation_reason = ?, updated_at = ?
                 WHERE id = ?
                   AND current_revision_id = ?",
            )
            .bind(encode_enum(&EscalationState::OperatorRequired)?)
            .bind(encode_enum(&escalation_reason)?)
            .bind(Utc::now())
            .bind(item_id.to_string())
            .bind(expected_item_revision_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

            if escalation.rows_affected() != 1 {
                return Err(RepositoryError::Conflict("job_revision_stale".into()));
            }
        }

        tx.commit().await.map_err(db_err)?;

        Ok(())
    }

    pub async fn dismiss_finding(&self, finding: &Finding) -> Result<(), RepositoryError> {
        sqlx::query(
            "UPDATE findings
             SET triage_state = ?, dismissal_reason = ?, triaged_at = ?, promoted_item_id = NULL
             WHERE id = ?",
        )
        .bind(encode_enum(&finding.triage_state)?)
        .bind(finding.dismissal_reason.as_deref())
        .bind(finding.triaged_at)
        .bind(finding.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(db_err)?;

        Ok(())
    }

    pub async fn promote_finding(
        &self,
        finding: &Finding,
        promoted_item: &Item,
        promoted_revision: &ItemRevision,
    ) -> Result<(), RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                done_reason, resolution_source, approval_state, escalation_state, escalation_reason,
                current_revision_id, origin_kind, origin_finding_id, priority, labels, operator_notes,
                created_at, updated_at, closed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(promoted_item.id.to_string())
        .bind(promoted_item.project_id.to_string())
        .bind(encode_enum(&promoted_item.classification)?)
        .bind(&promoted_item.workflow_version)
        .bind(encode_enum(&promoted_item.lifecycle_state)?)
        .bind(encode_enum(&promoted_item.parking_state)?)
        .bind(promoted_item.done_reason.as_ref().map(encode_enum).transpose()?)
        .bind(promoted_item.resolution_source.as_ref().map(encode_enum).transpose()?)
        .bind(encode_enum(&promoted_item.approval_state)?)
        .bind(encode_enum(&promoted_item.escalation_state)?)
        .bind(promoted_item.escalation_reason.as_ref().map(encode_enum).transpose()?)
        .bind(promoted_item.current_revision_id.to_string())
        .bind(encode_enum(&promoted_item.origin_kind)?)
        .bind(promoted_item.origin_finding_id.map(|id| id.to_string()))
        .bind(encode_enum(&promoted_item.priority)?)
        .bind(serde_json::to_string(&promoted_item.labels).map_err(json_err)?)
        .bind(promoted_item.operator_notes.as_deref())
        .bind(promoted_item.created_at)
        .bind(promoted_item.updated_at)
        .bind(promoted_item.closed_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(promoted_revision.id.to_string())
        .bind(promoted_revision.item_id.to_string())
        .bind(promoted_revision.revision_no as i64)
        .bind(&promoted_revision.title)
        .bind(&promoted_revision.description)
        .bind(&promoted_revision.acceptance_criteria)
        .bind(&promoted_revision.target_ref)
        .bind(encode_enum(&promoted_revision.approval_policy)?)
        .bind(serde_json::to_string(&promoted_revision.policy_snapshot).map_err(json_err)?)
        .bind(serde_json::to_string(&promoted_revision.template_map_snapshot).map_err(json_err)?)
        .bind(&promoted_revision.seed_commit_oid)
        .bind(promoted_revision.seed_target_commit_oid.as_deref())
        .bind(
            promoted_revision
                .supersedes_revision_id
                .map(|id| id.to_string()),
        )
        .bind(promoted_revision.created_at)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        sqlx::query(
            "UPDATE findings
             SET triage_state = 'promoted', promoted_item_id = ?, dismissal_reason = NULL, triaged_at = ?
             WHERE id = ?",
        )
        .bind(promoted_item.id.to_string())
        .bind(finding.triaged_at)
        .bind(finding.id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

impl JobCompletionRepository for Database {
    async fn load_job_completion_context(
        &self,
        job_id: JobId,
    ) -> Result<JobCompletionContext, RepositoryError> {
        Database::load_job_completion_context(self, job_id).await
    }

    async fn load_completed_job_completion(
        &self,
        job_id: JobId,
    ) -> Result<Option<CompletedJobCompletion>, RepositoryError> {
        Database::load_completed_job_completion(self, job_id).await
    }

    async fn apply_job_completion(
        &self,
        mutation: JobCompletionMutation,
    ) -> Result<(), RepositoryError> {
        Database::apply_job_completion(self, mutation).await
    }
}

async fn upsert_finding(
    tx: &mut Transaction<'_, Sqlite>,
    finding: &Finding,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO findings (
            id, project_id, source_item_id, source_item_revision_id, source_job_id, source_step_id,
            source_report_schema_version, source_finding_key, source_subject_kind,
            source_subject_base_commit_oid, source_subject_head_commit_oid, code, severity, summary,
            paths, evidence, triage_state, promoted_item_id, dismissal_reason, created_at, triaged_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(source_job_id, source_finding_key) DO UPDATE SET
            source_step_id = excluded.source_step_id,
            source_report_schema_version = excluded.source_report_schema_version,
            source_subject_kind = excluded.source_subject_kind,
            source_subject_base_commit_oid = excluded.source_subject_base_commit_oid,
            source_subject_head_commit_oid = excluded.source_subject_head_commit_oid,
            code = excluded.code,
            severity = excluded.severity,
            summary = excluded.summary,
            paths = excluded.paths,
            evidence = excluded.evidence",
    )
    .bind(finding.id.to_string())
    .bind(finding.project_id.to_string())
    .bind(finding.source_item_id.to_string())
    .bind(finding.source_item_revision_id.to_string())
    .bind(finding.source_job_id.to_string())
    .bind(&finding.source_step_id)
    .bind(&finding.source_report_schema_version)
    .bind(&finding.source_finding_key)
    .bind(encode_enum(&finding.source_subject_kind)?)
    .bind(finding.source_subject_base_commit_oid.as_deref())
    .bind(&finding.source_subject_head_commit_oid)
    .bind(&finding.code)
    .bind(encode_enum(&finding.severity)?)
    .bind(&finding.summary)
    .bind(serde_json::to_string(&finding.paths).map_err(json_err)?)
    .bind(serde_json::to_string(&finding.evidence).map_err(json_err)?)
    .bind(encode_enum(&finding.triage_state)?)
    .bind(finding.promoted_item_id.map(|id| id.to_string()))
    .bind(finding.dismissal_reason.as_deref())
    .bind(finding.created_at)
    .bind(finding.triaged_at)
    .execute(&mut **tx)
    .await
    .map_err(db_err)?;

    Ok(())
}

async fn classify_job_completion_conflict(
    tx: &mut Transaction<'_, Sqlite>,
    mutation: &JobCompletionMutation,
) -> Result<RepositoryError, RepositoryError> {
    if item_revision_is_stale(tx, mutation.item_id, mutation.expected_item_revision_id).await? {
        return Ok(RepositoryError::Conflict("job_revision_stale".into()));
    }

    if let Some(prepared_convergence_guard) = mutation.prepared_convergence_guard.as_ref() {
        let prepared_convergence = sqlx::query(
            "SELECT id, target_ref, input_target_commit_oid
             FROM convergences
             WHERE item_revision_id = ?
               AND status = 'prepared'
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(prepared_convergence_guard.item_revision_id.to_string())
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_err)?;

        let Some(prepared_convergence) = prepared_convergence else {
            return Ok(RepositoryError::Conflict(
                "prepared_convergence_missing".into(),
            ));
        };

        let prepared_convergence_id: String = prepared_convergence.try_get("id").map_err(db_err)?;
        let prepared_target_ref: String =
            prepared_convergence.try_get("target_ref").map_err(db_err)?;
        let input_target_commit_oid: Option<String> = prepared_convergence
            .try_get("input_target_commit_oid")
            .map_err(db_err)?;
        if prepared_convergence_id != prepared_convergence_guard.convergence_id.to_string()
            || prepared_target_ref != prepared_convergence_guard.target_ref
            || input_target_commit_oid.as_deref()
                != Some(prepared_convergence_guard.expected_target_head_oid.as_str())
        {
            return Ok(RepositoryError::Conflict(
                "prepared_convergence_stale".into(),
            ));
        }
    }

    Ok(RepositoryError::Conflict("job_not_active".into()))
}

async fn classify_terminal_job_conflict(
    tx: &mut Transaction<'_, Sqlite>,
    job_id: JobId,
    item_id: ItemId,
    expected_item_revision_id: ItemRevisionId,
) -> Result<RepositoryError, RepositoryError> {
    if item_revision_is_stale(tx, item_id, expected_item_revision_id).await? {
        return Ok(RepositoryError::Conflict("job_revision_stale".into()));
    }

    let job_is_active: Option<String> = sqlx::query_scalar(
        "SELECT id
         FROM jobs
         WHERE id = ?
           AND status IN ('queued', 'assigned', 'running')",
    )
    .bind(job_id.to_string())
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_err)?;

    if job_is_active.is_none() {
        return Ok(RepositoryError::Conflict("job_not_active".into()));
    }

    Ok(RepositoryError::Conflict("job_update_conflict".into()))
}

async fn classify_running_job_conflict(
    pool: &sqlx::SqlitePool,
    job_id: JobId,
    item_id: ItemId,
    expected_item_revision_id: ItemRevisionId,
    expected_statuses: &[&str],
) -> Result<RepositoryError, RepositoryError> {
    let mut tx = pool.begin().await.map_err(db_err)?;

    if item_revision_is_stale(&mut tx, item_id, expected_item_revision_id).await? {
        return Ok(RepositoryError::Conflict("job_revision_stale".into()));
    }

    let query = format!(
        "SELECT id
         FROM jobs
         WHERE id = ?
           AND status IN ({})",
        expected_statuses
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut query = sqlx::query_scalar::<_, String>(&query).bind(job_id.to_string());
    for status in expected_statuses {
        query = query.bind(*status);
    }

    let job_matches = query.fetch_optional(&mut *tx).await.map_err(db_err)?;
    if job_matches.is_none() {
        return Ok(RepositoryError::Conflict("job_not_active".into()));
    }

    Ok(RepositoryError::Conflict("job_update_conflict".into()))
}

async fn item_revision_is_stale(
    tx: &mut Transaction<'_, Sqlite>,
    item_id: ItemId,
    expected_item_revision_id: ItemRevisionId,
) -> Result<bool, RepositoryError> {
    let expected_item_revision_id = expected_item_revision_id.to_string();
    let current_revision_id: Option<String> =
        sqlx::query_scalar("SELECT current_revision_id FROM items WHERE id = ?")
            .bind(item_id.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_err)?;

    Ok(current_revision_id.as_deref() != Some(expected_item_revision_id.as_str()))
}

fn map_project(row: &SqliteRow) -> Result<Project, RepositoryError> {
    Ok(Project {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        name: row.try_get("name").map_err(db_err)?,
        path: row.try_get("path").map_err(db_err)?,
        default_branch: row.try_get("default_branch").map_err(db_err)?,
        color: row.try_get("color").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        updated_at: row.try_get("updated_at").map_err(db_err)?,
    })
}

fn map_agent(row: &SqliteRow) -> Result<Agent, RepositoryError> {
    Ok(Agent {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        slug: row.try_get("slug").map_err(db_err)?,
        name: row.try_get("name").map_err(db_err)?,
        adapter_kind: parse_enum(row.try_get("adapter_kind").map_err(db_err)?)?,
        provider: row.try_get("provider").map_err(db_err)?,
        model: row.try_get("model").map_err(db_err)?,
        cli_path: row.try_get("cli_path").map_err(db_err)?,
        capabilities: parse_json(row.try_get("capabilities").map_err(db_err)?)?,
        health_check: row.try_get("health_check").map_err(db_err)?,
        status: parse_enum(row.try_get("status").map_err(db_err)?)?,
    })
}

fn map_item(row: &SqliteRow) -> Result<Item, RepositoryError> {
    Ok(Item {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        classification: parse_enum(row.try_get("classification").map_err(db_err)?)?,
        workflow_version: row.try_get("workflow_version").map_err(db_err)?,
        lifecycle_state: parse_enum(row.try_get("lifecycle_state").map_err(db_err)?)?,
        parking_state: parse_enum(row.try_get("parking_state").map_err(db_err)?)?,
        done_reason: row
            .try_get::<Option<String>, _>("done_reason")
            .map_err(db_err)?
            .map(parse_enum)
            .transpose()?,
        resolution_source: row
            .try_get::<Option<String>, _>("resolution_source")
            .map_err(db_err)?
            .map(parse_enum)
            .transpose()?,
        approval_state: parse_enum(row.try_get("approval_state").map_err(db_err)?)?,
        escalation_state: parse_enum(row.try_get("escalation_state").map_err(db_err)?)?,
        escalation_reason: row
            .try_get::<Option<String>, _>("escalation_reason")
            .map_err(db_err)?
            .map(parse_enum)
            .transpose()?,
        current_revision_id: parse_id(row.try_get("current_revision_id").map_err(db_err)?)?,
        origin_kind: parse_enum(row.try_get("origin_kind").map_err(db_err)?)?,
        origin_finding_id: row
            .try_get::<Option<String>, _>("origin_finding_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        priority: parse_enum(row.try_get("priority").map_err(db_err)?)?,
        labels: parse_json(row.try_get("labels").map_err(db_err)?)?,
        operator_notes: row.try_get("operator_notes").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        updated_at: row.try_get("updated_at").map_err(db_err)?,
        closed_at: row.try_get("closed_at").map_err(db_err)?,
    })
}

fn map_revision(row: &SqliteRow) -> Result<ItemRevision, RepositoryError> {
    Ok(ItemRevision {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        item_id: parse_id(row.try_get("item_id").map_err(db_err)?)?,
        revision_no: row.try_get::<i64, _>("revision_no").map_err(db_err)? as u32,
        title: row.try_get("title").map_err(db_err)?,
        description: row.try_get("description").map_err(db_err)?,
        acceptance_criteria: row.try_get("acceptance_criteria").map_err(db_err)?,
        target_ref: row.try_get("target_ref").map_err(db_err)?,
        approval_policy: parse_enum(row.try_get("approval_policy").map_err(db_err)?)?,
        policy_snapshot: parse_json(row.try_get("policy_snapshot").map_err(db_err)?)?,
        template_map_snapshot: parse_json(row.try_get("template_map_snapshot").map_err(db_err)?)?,
        seed_commit_oid: row.try_get("seed_commit_oid").map_err(db_err)?,
        seed_target_commit_oid: row.try_get("seed_target_commit_oid").map_err(db_err)?,
        supersedes_revision_id: row
            .try_get::<Option<String>, _>("supersedes_revision_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        created_at: row.try_get("created_at").map_err(db_err)?,
    })
}

fn map_revision_context(row: &SqliteRow) -> Result<RevisionContext, RepositoryError> {
    Ok(RevisionContext {
        item_revision_id: parse_id(row.try_get("item_revision_id").map_err(db_err)?)?,
        schema_version: row.try_get("schema_version").map_err(db_err)?,
        payload: parse_json(row.try_get("payload").map_err(db_err)?)?,
        updated_from_job_id: row
            .try_get::<Option<String>, _>("updated_from_job_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        updated_at: row.try_get("updated_at").map_err(db_err)?,
    })
}

fn map_job(row: &SqliteRow) -> Result<Job, RepositoryError> {
    Ok(Job {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        item_id: parse_id(row.try_get("item_id").map_err(db_err)?)?,
        item_revision_id: parse_id(row.try_get("item_revision_id").map_err(db_err)?)?,
        step_id: row.try_get("step_id").map_err(db_err)?,
        semantic_attempt_no: row
            .try_get::<i64, _>("semantic_attempt_no")
            .map_err(db_err)? as u32,
        retry_no: row.try_get::<i64, _>("retry_no").map_err(db_err)? as u32,
        supersedes_job_id: row
            .try_get::<Option<String>, _>("supersedes_job_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        status: parse_enum(row.try_get("status").map_err(db_err)?)?,
        outcome_class: row
            .try_get::<Option<String>, _>("outcome_class")
            .map_err(db_err)?
            .map(parse_enum)
            .transpose()?,
        phase_kind: parse_enum(row.try_get("phase_kind").map_err(db_err)?)?,
        workspace_id: row
            .try_get::<Option<String>, _>("workspace_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        workspace_kind: parse_enum(row.try_get("workspace_kind").map_err(db_err)?)?,
        execution_permission: parse_enum(row.try_get("execution_permission").map_err(db_err)?)?,
        context_policy: parse_enum(row.try_get("context_policy").map_err(db_err)?)?,
        phase_template_slug: row.try_get("phase_template_slug").map_err(db_err)?,
        phase_template_digest: row.try_get("phase_template_digest").map_err(db_err)?,
        prompt_snapshot: row.try_get("prompt_snapshot").map_err(db_err)?,
        input_base_commit_oid: row.try_get("input_base_commit_oid").map_err(db_err)?,
        input_head_commit_oid: row.try_get("input_head_commit_oid").map_err(db_err)?,
        output_artifact_kind: parse_enum(row.try_get("output_artifact_kind").map_err(db_err)?)?,
        output_commit_oid: row.try_get("output_commit_oid").map_err(db_err)?,
        result_schema_version: row.try_get("result_schema_version").map_err(db_err)?,
        result_payload: row
            .try_get::<Option<String>, _>("result_payload")
            .map_err(db_err)?
            .map(parse_json)
            .transpose()?,
        agent_id: row
            .try_get::<Option<String>, _>("agent_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        process_pid: row
            .try_get::<Option<i64>, _>("process_pid")
            .map_err(db_err)?
            .map(|value| value as u32),
        lease_owner_id: row.try_get("lease_owner_id").map_err(db_err)?,
        heartbeat_at: row.try_get("heartbeat_at").map_err(db_err)?,
        lease_expires_at: row.try_get("lease_expires_at").map_err(db_err)?,
        error_code: row.try_get("error_code").map_err(db_err)?,
        error_message: row.try_get("error_message").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        started_at: row.try_get("started_at").map_err(db_err)?,
        ended_at: row.try_get("ended_at").map_err(db_err)?,
    })
}

fn map_workspace(row: &SqliteRow) -> Result<Workspace, RepositoryError> {
    Ok(Workspace {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        kind: parse_enum(row.try_get("kind").map_err(db_err)?)?,
        strategy: parse_enum(row.try_get("strategy").map_err(db_err)?)?,
        path: row.try_get("path").map_err(db_err)?,
        created_for_revision_id: row
            .try_get::<Option<String>, _>("created_for_revision_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        parent_workspace_id: row
            .try_get::<Option<String>, _>("parent_workspace_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        target_ref: row.try_get("target_ref").map_err(db_err)?,
        workspace_ref: row.try_get("workspace_ref").map_err(db_err)?,
        base_commit_oid: row.try_get("base_commit_oid").map_err(db_err)?,
        head_commit_oid: row.try_get("head_commit_oid").map_err(db_err)?,
        retention_policy: parse_enum(row.try_get("retention_policy").map_err(db_err)?)?,
        status: parse_enum(row.try_get("status").map_err(db_err)?)?,
        current_job_id: row
            .try_get::<Option<String>, _>("current_job_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        updated_at: row.try_get("updated_at").map_err(db_err)?,
    })
}

fn map_convergence(row: &SqliteRow) -> Result<Convergence, RepositoryError> {
    Ok(Convergence {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        item_id: parse_id(row.try_get("item_id").map_err(db_err)?)?,
        item_revision_id: parse_id(row.try_get("item_revision_id").map_err(db_err)?)?,
        source_workspace_id: parse_id(row.try_get("source_workspace_id").map_err(db_err)?)?,
        integration_workspace_id: row
            .try_get::<Option<String>, _>("integration_workspace_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        source_head_commit_oid: row.try_get("source_head_commit_oid").map_err(db_err)?,
        target_ref: row.try_get("target_ref").map_err(db_err)?,
        strategy: parse_enum(row.try_get("strategy").map_err(db_err)?)?,
        status: parse_enum(row.try_get("status").map_err(db_err)?)?,
        input_target_commit_oid: row.try_get("input_target_commit_oid").map_err(db_err)?,
        prepared_commit_oid: row.try_get("prepared_commit_oid").map_err(db_err)?,
        final_target_commit_oid: row.try_get("final_target_commit_oid").map_err(db_err)?,
        target_head_valid: None,
        conflict_summary: row.try_get("conflict_summary").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        completed_at: row.try_get("completed_at").map_err(db_err)?,
    })
}

fn map_finding(row: &SqliteRow) -> Result<Finding, RepositoryError> {
    Ok(Finding {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        source_item_id: parse_id(row.try_get("source_item_id").map_err(db_err)?)?,
        source_item_revision_id: parse_id(row.try_get("source_item_revision_id").map_err(db_err)?)?,
        source_job_id: parse_id(row.try_get("source_job_id").map_err(db_err)?)?,
        source_step_id: row.try_get("source_step_id").map_err(db_err)?,
        source_report_schema_version: row
            .try_get("source_report_schema_version")
            .map_err(db_err)?,
        source_finding_key: row.try_get("source_finding_key").map_err(db_err)?,
        source_subject_kind: parse_enum(row.try_get("source_subject_kind").map_err(db_err)?)?,
        source_subject_base_commit_oid: row
            .try_get("source_subject_base_commit_oid")
            .map_err(db_err)?,
        source_subject_head_commit_oid: row
            .try_get("source_subject_head_commit_oid")
            .map_err(db_err)?,
        code: row.try_get("code").map_err(db_err)?,
        severity: parse_enum(row.try_get("severity").map_err(db_err)?)?,
        summary: row.try_get("summary").map_err(db_err)?,
        paths: parse_json(row.try_get("paths").map_err(db_err)?)?,
        evidence: parse_json(row.try_get("evidence").map_err(db_err)?)?,
        triage_state: parse_enum(row.try_get("triage_state").map_err(db_err)?)?,
        promoted_item_id: row
            .try_get::<Option<String>, _>("promoted_item_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        dismissal_reason: row.try_get("dismissal_reason").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        triaged_at: row.try_get("triaged_at").map_err(db_err)?,
    })
}

#[allow(dead_code)]
fn map_git_operation(row: &SqliteRow) -> Result<GitOperation, RepositoryError> {
    Ok(GitOperation {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        operation_kind: parse_enum(row.try_get("operation_kind").map_err(db_err)?)?,
        entity_type: parse_enum(row.try_get("entity_type").map_err(db_err)?)?,
        entity_id: row.try_get("entity_id").map_err(db_err)?,
        workspace_id: row
            .try_get::<Option<String>, _>("workspace_id")
            .map_err(db_err)?
            .map(parse_id)
            .transpose()?,
        ref_name: row.try_get("ref_name").map_err(db_err)?,
        expected_old_oid: row.try_get("expected_old_oid").map_err(db_err)?,
        new_oid: row.try_get("new_oid").map_err(db_err)?,
        commit_oid: row.try_get("commit_oid").map_err(db_err)?,
        status: parse_enum(row.try_get("status").map_err(db_err)?)?,
        metadata: row
            .try_get::<Option<String>, _>("metadata")
            .map_err(db_err)?
            .map(parse_json)
            .transpose()?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        completed_at: row.try_get("completed_at").map_err(db_err)?,
    })
}

fn map_activity(row: &SqliteRow) -> Result<Activity, RepositoryError> {
    Ok(Activity {
        id: parse_id(row.try_get("id").map_err(db_err)?)?,
        project_id: parse_id(row.try_get("project_id").map_err(db_err)?)?,
        event_type: parse_enum(row.try_get("event_type").map_err(db_err)?)?,
        entity_type: row.try_get("entity_type").map_err(db_err)?,
        entity_id: row.try_get("entity_id").map_err(db_err)?,
        payload: parse_json(row.try_get("payload").map_err(db_err)?)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
    })
}

fn parse_enum<T>(value: String) -> Result<T, RepositoryError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(value.clone())).map_err(|err| {
        RepositoryError::Database(Box::new(StoreDecodeError::Enum {
            value,
            message: err.to_string(),
        }))
    })
}

fn encode_enum<T>(value: &T) -> Result<String, RepositoryError>
where
    T: Serialize,
{
    match serde_json::to_value(value).map_err(json_err)? {
        serde_json::Value::String(value) => Ok(value),
        other => Err(RepositoryError::Database(Box::new(StoreDecodeError::Json(
            format!("expected string serialization, got {other}"),
        )))),
    }
}

fn parse_json<T>(value: String) -> Result<T, RepositoryError>
where
    T: DeserializeOwned,
{
    serde_json::from_str(&value).map_err(|err| {
        RepositoryError::Database(Box::new(StoreDecodeError::Json(format!("{value}: {err}"))))
    })
}

fn parse_id<T>(value: String) -> Result<T, RepositoryError>
where
    T: FromStr,
    <T as FromStr>::Err: std::error::Error + Send + Sync + 'static,
{
    value.parse().map_err(|err: <T as FromStr>::Err| {
        RepositoryError::Database(Box::new(StoreDecodeError::Id {
            value,
            message: err.to_string(),
        }))
    })
}

fn serialize_optional_json(
    value: Option<&serde_json::Value>,
) -> Result<Option<String>, RepositoryError> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(json_err)
}

fn db_err<E>(err: E) -> RepositoryError
where
    E: std::error::Error + Send + Sync + 'static,
{
    RepositoryError::Database(Box::new(err))
}

fn db_write_err(err: sqlx::Error) -> RepositoryError {
    match err {
        sqlx::Error::Database(database_error)
            if database_error.is_unique_violation()
                || database_error.is_foreign_key_violation() =>
        {
            RepositoryError::Conflict(database_error.message().to_string())
        }
        other => db_err(other),
    }
}

fn json_err(err: serde_json::Error) -> RepositoryError {
    RepositoryError::Database(Box::new(err))
}
