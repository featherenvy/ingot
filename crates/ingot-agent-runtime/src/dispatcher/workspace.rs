use super::*;

impl JobDispatcher {
    pub(super) async fn finalize_workspace_after_success(
        &self,
        prepared: &PreparedRun,
        head_commit_oid: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(prepared.job.project_id)
            .await;

        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                let now = Utc::now();
                workspace.release_to(WorkspaceStatus::Ready, now);
                if let Some(head_commit_oid) = head_commit_oid {
                    workspace.set_head_commit_oid(head_commit_oid.to_string(), now);
                }
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.release_to(WorkspaceStatus::Ready, Utc::now());
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                remove_workspace(
                    prepared.canonical_repo_path.as_path(),
                    Path::new(&prepared.workspace.path),
                )
                .await?;
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.mark_abandoned(Utc::now());
                self.db.update_workspace(&workspace).await?;
            }
        }

        Ok(())
    }

    pub(super) async fn finalize_integration_workspace_after_close(
        &self,
        project: &Project,
        workspace: &Workspace,
    ) -> Result<(), RuntimeError> {
        let repo_path = self.project_paths(project).mirror_git_dir;
        remove_workspace(repo_path.as_path(), Path::new(&workspace.path)).await?;
        let mut workspace = workspace.clone();
        workspace.mark_abandoned(Utc::now());
        self.db.update_workspace(&workspace).await?;
        Ok(())
    }

    pub(super) async fn finalize_workspace_after_failure(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(prepared.job.project_id)
            .await;

        self.reset_workspace(prepared).await?;

        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                let now = Utc::now();
                workspace.release_with_head(prepared.original_head_commit_oid.clone(), now);
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                let now = Utc::now();
                workspace.release_with_head(prepared.original_head_commit_oid.clone(), now);
                self.db.update_workspace(&workspace).await?;
            }
            WorkspaceLifecycle::EphemeralReview => {
                let mut workspace = self.db.get_workspace(prepared.workspace.id).await?;
                workspace.mark_abandoned(Utc::now());
                self.db.update_workspace(&workspace).await?;
            }
        }

        Ok(())
    }

    pub(super) async fn reset_workspace(&self, prepared: &PreparedRun) -> Result<(), RuntimeError> {
        match prepared.workspace_lifecycle {
            WorkspaceLifecycle::PersistentAuthoring => {
                let workspace_path = Path::new(&prepared.workspace.path);
                git(
                    workspace_path,
                    &["reset", "--hard", &prepared.original_head_commit_oid],
                )
                .await?;
                git(workspace_path, &["clean", "-fd"]).await?;
                if let Some(workspace_ref) = prepared.workspace.workspace_ref.as_deref() {
                    git(
                        prepared.canonical_repo_path.as_path(),
                        &[
                            "update-ref",
                            workspace_ref,
                            &prepared.original_head_commit_oid,
                        ],
                    )
                    .await?;
                }
            }
            WorkspaceLifecycle::PersistentIntegration => {
                let workspace_path = Path::new(&prepared.workspace.path);
                git(
                    workspace_path,
                    &["reset", "--hard", &prepared.original_head_commit_oid],
                )
                .await?;
                git(workspace_path, &["clean", "-fd"]).await?;
                if let Some(workspace_ref) = prepared.workspace.workspace_ref.as_deref() {
                    git(
                        prepared.canonical_repo_path.as_path(),
                        &[
                            "update-ref",
                            workspace_ref,
                            &prepared.original_head_commit_oid,
                        ],
                    )
                    .await?;
                }
            }
            WorkspaceLifecycle::EphemeralReview => {
                remove_workspace(
                    prepared.canonical_repo_path.as_path(),
                    Path::new(&prepared.workspace.path),
                )
                .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn workspace_can_be_removed(
        &self,
        _project: &Project,
        workspace: &Workspace,
    ) -> Result<bool, RuntimeError> {
        if workspace.kind == WorkspaceKind::Review {
            return Ok(true);
        }
        let Some(revision_id) = workspace.created_for_revision_id else {
            return Ok(true);
        };
        let revision = self.db.get_revision(revision_id).await?;
        let item = self.db.get_item(revision.item_id).await?;
        if matches!(
            workspace.kind,
            WorkspaceKind::Authoring | WorkspaceKind::Integration
        ) && item.current_revision_id == revision.id
            && item.lifecycle.is_open()
        {
            return Ok(false);
        }

        let findings = self.db.list_findings_by_item(item.id).await?;
        let head_commit_oid = workspace.state.head_commit_oid().unwrap_or_default();
        let blocked = findings.iter().any(|finding| {
            finding.source_item_revision_id == revision.id
                && finding.triage.is_unresolved()
                && finding.source_subject_head_commit_oid == head_commit_oid
                && match workspace.kind {
                    WorkspaceKind::Authoring => {
                        finding.source_subject_kind
                            == ingot_domain::finding::FindingSubjectKind::Candidate
                    }
                    WorkspaceKind::Integration => {
                        finding.source_subject_kind
                            == ingot_domain::finding::FindingSubjectKind::Integrated
                    }
                    WorkspaceKind::Review => false,
                }
        });

        Ok(!blocked)
    }

    pub(super) async fn remove_abandoned_workspace(
        &self,
        project: &Project,
        workspace: &Workspace,
    ) -> Result<(), RuntimeError> {
        let repo_path = self.project_paths(project).mirror_git_dir;
        let path = Path::new(&workspace.path);
        if path.exists() {
            remove_workspace(repo_path.as_path(), path).await?;
        }

        if let Some(workspace_ref) = workspace.workspace_ref.as_deref()
            && let Some(current_oid) = resolve_ref_oid(repo_path.as_path(), workspace_ref).await?
        {
            let mut operation = GitOperation {
                id: GitOperationId::new(),
                project_id: project.id,
                entity_id: workspace.id.to_string(),
                payload: OperationPayload::RemoveWorkspaceRef {
                    workspace_id: workspace.id,
                    ref_name: workspace_ref.into(),
                    expected_old_oid: current_oid,
                },
                status: GitOperationStatus::Planned,
                created_at: Utc::now(),
                completed_at: None,
            };
            self.db.create_git_operation(&operation).await?;
            self.append_activity(
                project.id,
                ActivityEventType::GitOperationPlanned,
                "git_operation",
                operation.id.to_string(),
                serde_json::json!({ "operation_kind": operation.operation_kind(), "entity_id": operation.entity_id }),
            )
            .await?;
            delete_ref(repo_path.as_path(), workspace_ref).await?;
            operation.status = GitOperationStatus::Applied;
            operation.completed_at = Some(Utc::now());
            self.db.update_git_operation(&operation).await?;
        }

        Ok(())
    }
}
