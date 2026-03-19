use super::*;
use crate::dispatcher::completion::is_closure_relevant_job;

impl JobDispatcher {
    pub(super) async fn assemble_prompt(
        &self,
        job: &Job,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        template: &str,
        harness_prompt: &HarnessPromptContext,
    ) -> Result<String, RuntimeError> {
        let revision_context = self.db.get_revision_context(revision.id).await?;
        let context_block = format_revision_context(revision_context.as_ref());
        let workspace_kind = match job.workspace_kind {
            WorkspaceKind::Authoring => "authoring",
            WorkspaceKind::Review => "review",
            WorkspaceKind::Integration => "integration",
        };
        let execution = match job.execution_permission {
            ExecutionPermission::MayMutate => "may_mutate",
            ExecutionPermission::MustNotMutate => "must_not_mutate",
            ExecutionPermission::DaemonOnly => "daemon_only",
        };

        let mut prompt = format!(
            "Revision contract:\n- Item ID: {}\n- Revision: {}\n- Title: {}\n- Description: {}\n- Acceptance criteria: {}\n- Target ref: {}\n- Approval policy: {:?}\n\nWorkflow step:\n- Step: {}\n- Template: {}\n- Workspace: {}\n- Execution: {}\n",
            item.id,
            revision.revision_no,
            revision.title,
            revision.description,
            revision.acceptance_criteria,
            revision.target_ref,
            revision.approval_policy,
            job.step_id,
            job.phase_template_slug,
            workspace_kind,
            execution,
        );

        if let Some(base) = job.job_input.base_commit_oid() {
            prompt.push_str(&format!("- Input base commit: {base}\n"));
        }
        if let Some(head) = job.job_input.head_commit_oid() {
            prompt.push_str(&format!("- Input head commit: {head}\n"));
        }

        prompt.push_str(&format!(
            "\nTemplate prompt:\n{}\n\nRevision context:\n{}\n\n",
            template, context_block
        ));

        if matches!(
            job.step_id.as_str(),
            "repair_candidate" | "repair_after_integration"
        ) {
            let jobs = self.db.list_jobs_by_item(item.id).await?;
            let findings = self.db.list_findings_by_item(item.id).await?;
            let latest_closure_findings_job = jobs
                .iter()
                .filter(|candidate| candidate.item_revision_id == revision.id)
                .filter(|candidate| candidate.state.status().is_terminal())
                .filter(|candidate| candidate.state.outcome_class() == Some(OutcomeClass::Findings))
                .filter(|candidate| is_closure_relevant_job(candidate))
                .max_by_key(|candidate| (candidate.state.ended_at(), candidate.created_at));

            if let Some(latest_job) = latest_closure_findings_job {
                let scoped_findings = findings
                    .iter()
                    .filter(|finding| finding.source_item_revision_id == revision.id)
                    .filter(|finding| finding.source_job_id == latest_job.id)
                    .collect::<Vec<_>>();
                let fix_now_findings = scoped_findings
                    .iter()
                    .filter(|finding| finding.triage.state() == FindingTriageState::FixNow)
                    .collect::<Vec<_>>();
                let accepted_findings = scoped_findings
                    .iter()
                    .filter(|finding| !finding.triage.blocks_closure())
                    .collect::<Vec<_>>();

                if !fix_now_findings.is_empty() || !accepted_findings.is_empty() {
                    prompt.push_str("Finding triage for this repair:\n");
                }
                if !fix_now_findings.is_empty() {
                    prompt.push_str("- Fix now findings:\n");
                    for finding in &fix_now_findings {
                        prompt.push_str(&format!(
                            "  - [{}] {} ({:?})\n",
                            finding.code, finding.summary, finding.severity
                        ));
                    }
                }
                if !accepted_findings.is_empty() {
                    prompt.push_str("- Already triaged as non-blocking for this attempt:\n");
                    for finding in &accepted_findings {
                        prompt.push_str(&format!(
                            "  - [{}] {} => {:?}\n",
                            finding.code,
                            finding.summary,
                            finding.triage.state()
                        ));
                    }
                }
                if !fix_now_findings.is_empty() || !accepted_findings.is_empty() {
                    prompt.push('\n');
                }
            }
        }

        match job.output_artifact_kind {
            OutputArtifactKind::Commit => {
                prompt.push_str(
                    "Protocol:\n- Edit files inside the current repository to satisfy the revision contract.\n- You may run local validation commands when useful.\n- Do not create commits, amend commits, rebase, merge, cherry-pick, or move refs.\n- Leave all changes unstaged or staged in the working tree; Ingot will create the canonical commit.\n- Return a structured object with keys `summary` and `validation`; set `validation` to null when no validation was run.\n",
                );
            }
            OutputArtifactKind::ReviewReport
            | OutputArtifactKind::ValidationReport
            | OutputArtifactKind::FindingReport => {
                prompt.push_str(
                    "Protocol:\n- Do not modify files, create commits, rebase, merge, cherry-pick, or move refs.\n- Inspect the current workspace subject and produce only the canonical structured report for this step.\n- Any non-core data must go under `extensions`.\n",
                );
                prompt.push_str(report_prompt_suffix(job));
            }
            OutputArtifactKind::None => {
                prompt.push_str("Protocol:\n- No output artifact is expected for this step.\n");
            }
        }

        if !harness_prompt.commands.is_empty() {
            prompt.push_str("\nAvailable verification commands:\n");
            for cmd in &harness_prompt.commands {
                prompt.push_str(&format!("- `{}`: `{}`\n", cmd.name, cmd.run));
            }
        }
        if !harness_prompt.skills.is_empty() {
            prompt.push_str("\nRepo-local skills available:\n");
            for skill in &harness_prompt.skills {
                prompt.push_str(&format!(
                    "\nSkill file: {}\n{}\n",
                    skill.relative_path, skill.contents
                ));
            }
        }

        Ok(prompt)
    }
}

pub(super) fn read_harness_profile_if_present(
    project_path: &Path,
) -> Result<Option<HarnessProfile>, HarnessLoadError> {
    let path = project_path.join(".ingot/harness.toml");
    match std::fs::read_to_string(&path) {
        Ok(content) => HarnessProfile::from_toml(&content)
            .map(Some)
            .map_err(|source| HarnessLoadError::InvalidProfile { path, source }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(HarnessLoadError::ReadProfile { path, source }),
    }
}

pub(super) fn load_harness_profile(
    project_path: &Path,
) -> Result<HarnessProfile, HarnessLoadError> {
    Ok(read_harness_profile_if_present(project_path)?.unwrap_or_default())
}

pub(super) fn resolve_harness_prompt_context(
    project_path: &Path,
) -> Result<HarnessPromptContext, HarnessLoadError> {
    let harness = load_harness_profile(project_path)?;
    Ok(HarnessPromptContext {
        commands: harness.commands,
        skills: resolve_harness_skills(project_path, &harness.skills.paths)?,
    })
}

fn resolve_harness_skills(
    project_path: &Path,
    patterns: &[String],
) -> Result<Vec<ResolvedHarnessSkill>, HarnessLoadError> {
    let canonical_project_path = std::fs::canonicalize(project_path).map_err(|source| {
        HarnessLoadError::CanonicalizeProjectPath {
            path: project_path.to_path_buf(),
            source,
        }
    })?;
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();
    for pattern in patterns {
        let pattern_path = project_path.join(pattern);
        let pattern_glob = pattern_path.to_string_lossy().into_owned();
        let mut matches = Vec::new();
        for entry in glob(&pattern_glob).map_err(|error| HarnessLoadError::InvalidSkillGlob {
            pattern: pattern.clone(),
            message: error.msg.to_string(),
        })? {
            match entry {
                Ok(path) => matches.push(path),
                Err(error) => {
                    return Err(HarnessLoadError::ResolveSkillPath {
                        pattern: pattern.clone(),
                        source: io::Error::new(error.error().kind(), error.error().to_string()),
                    });
                }
            }
        }
        matches.sort();
        for path in matches {
            if !path.is_file() {
                continue;
            }
            let canonical_path = std::fs::canonicalize(&path).map_err(|source| {
                HarnessLoadError::ResolveSkillPath {
                    pattern: pattern.clone(),
                    source,
                }
            })?;
            let relative_path = canonical_path
                .strip_prefix(&canonical_project_path)
                .map_err(|_| HarnessLoadError::SkillPathEscapesProjectRoot {
                    pattern: pattern.clone(),
                    project_path: canonical_project_path.clone(),
                    path: canonical_path.clone(),
                })?
                .display()
                .to_string();
            if !seen.insert(relative_path.clone()) {
                continue;
            }
            let contents = std::fs::read_to_string(&canonical_path).map_err(|source| {
                HarnessLoadError::ReadSkill {
                    path: canonical_path.clone(),
                    source,
                }
            })?;
            resolved.push(ResolvedHarnessSkill {
                relative_path,
                contents,
            });
        }
    }
    Ok(resolved)
}

pub(super) fn built_in_template(template_slug: &str, step_id: &str) -> &'static str {
    match template_slug {
        "author-initial" => {
            "Implement the requested change directly in the repository. Keep the edit set focused on the acceptance criteria and preserve surrounding style."
        }
        "repair-candidate" | "repair-integrated" => {
            "Repair the current candidate based on the latest validation or review feedback while preserving the accepted parts of the prior work."
        }
        "review-incremental" => {
            "Review only the requested incremental diff and report concrete findings against the exact review subject."
        }
        "review-candidate" => {
            "Review the full candidate diff from the seed commit to the current head and report concrete findings when necessary."
        }
        "validate-candidate" | "validate-integrated" => {
            "Run objective validation against the current workspace subject and report failed checks or findings only when they are real."
        }
        "investigate-item" => {
            "Investigate the current subject and produce a finding report only when there is a concrete issue worth tracking."
        }
        _ => match step_id {
            "author_initial" => {
                "Implement the requested change directly in the repository. Keep the edit set focused on the acceptance criteria and preserve surrounding style."
            }
            "review_incremental_initial"
            | "review_incremental_repair"
            | "review_incremental_after_integration_repair" => {
                "Review only the requested incremental diff and report concrete findings against the exact review subject."
            }
            "review_candidate_initial"
            | "review_candidate_repair"
            | "review_after_integration_repair" => {
                "Review the full candidate diff from the seed commit to the current head and report concrete findings when necessary."
            }
            "validate_candidate_initial"
            | "validate_candidate_repair"
            | "validate_after_integration_repair"
            | "validate_integrated" => {
                "Run objective validation against the current workspace subject and report failed checks or findings only when they are real."
            }
            "investigate_item" => {
                "Investigate the current subject and produce a finding report only when there is a concrete issue worth tracking."
            }
            _ => {
                "Update the repository for the current authoring step and keep the change set narrowly scoped to the revision contract."
            }
        },
    }
}

pub(super) fn report_prompt_suffix(job: &Job) -> &'static str {
    match job.output_artifact_kind {
        OutputArtifactKind::ValidationReport => {
            "Return JSON matching `validation_report:v1` with keys `outcome`, `summary`, `checks`, `findings`, and `extensions`. Set `extensions` to null when unused. Use `outcome=clean` only when there are no failed checks and no findings."
        }
        OutputArtifactKind::ReviewReport => {
            "Return JSON matching `review_report:v1` with keys `outcome`, `summary`, `review_subject`, `overall_risk`, `findings`, and `extensions`. Set `extensions` to null when unused. The `review_subject.base_commit_oid` and `review_subject.head_commit_oid` must exactly match the provided input commits."
        }
        OutputArtifactKind::FindingReport => {
            "Return JSON matching `finding_report:v1` with keys `outcome`, `summary`, `findings`, and `extensions`. Set `extensions` to null when unused."
        }
        _ => "",
    }
}

pub(super) fn output_schema_for_job(job: &Job) -> Option<serde_json::Value> {
    match job.output_artifact_kind {
        OutputArtifactKind::Commit => Some(commit_summary_schema()),
        OutputArtifactKind::ValidationReport => Some(validation_report_schema()),
        OutputArtifactKind::ReviewReport => Some(review_report_schema()),
        OutputArtifactKind::FindingReport => Some(finding_report_schema()),
        OutputArtifactKind::None => None,
    }
}

pub(super) fn result_schema_version(
    output_artifact_kind: OutputArtifactKind,
) -> Option<&'static str> {
    match output_artifact_kind {
        OutputArtifactKind::ValidationReport => Some("validation_report:v1"),
        OutputArtifactKind::ReviewReport => Some("review_report:v1"),
        OutputArtifactKind::FindingReport => Some("finding_report:v1"),
        _ => None,
    }
}

pub(super) fn report_outcome_class(
    result_payload: &serde_json::Value,
) -> Result<OutcomeClass, String> {
    match result_payload
        .get("outcome")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
    {
        "clean" => Ok(OutcomeClass::Clean),
        "findings" => Ok(OutcomeClass::Findings),
        other => Err(format!("unsupported report outcome `{other}`")),
    }
}

fn commit_summary_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "validation": {
                "type": ["string", "null"]
            }
        },
        "required": ["summary", "validation"],
        "additionalProperties": false
    })
}

fn finding_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "finding_key": { "type": "string" },
            "code": { "type": "string" },
            "severity": { "type": "string", "enum": ["low", "medium", "high", "critical"] },
            "summary": { "type": "string" },
            "paths": {
                "type": "array",
                "items": { "type": "string" }
            },
            "evidence": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["finding_key", "code", "severity", "summary", "paths", "evidence"],
        "additionalProperties": false
    })
}

fn nullable_closed_extensions_schema() -> serde_json::Value {
    serde_json::json!({
        "anyOf": [
            {
                "type": "object",
                "additionalProperties": false
            },
            {
                "type": "null"
            }
        ]
    })
}

fn validation_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "checks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "status": { "type": "string", "enum": ["pass", "fail", "skip"] },
                        "summary": { "type": "string" }
                    },
                    "required": ["name", "status", "summary"],
                    "additionalProperties": false
                }
            },
            "findings": {
                "type": "array",
                "items": finding_schema()
            },
            "extensions": nullable_closed_extensions_schema()
        },
        "required": ["outcome", "summary", "checks", "findings", "extensions"],
        "additionalProperties": false
    })
}

fn review_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "review_subject": {
                "type": "object",
                "properties": {
                    "base_commit_oid": { "type": "string" },
                    "head_commit_oid": { "type": "string" }
                },
                "required": ["base_commit_oid", "head_commit_oid"],
                "additionalProperties": false
            },
            "overall_risk": { "type": "string", "enum": ["low", "medium", "high"] },
            "findings": {
                "type": "array",
                "items": finding_schema()
            },
            "extensions": nullable_closed_extensions_schema()
        },
        "required": ["outcome", "summary", "review_subject", "overall_risk", "findings", "extensions"],
        "additionalProperties": false
    })
}

fn finding_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["clean", "findings"] },
            "summary": { "type": "string" },
            "findings": {
                "type": "array",
                "items": finding_schema()
            },
            "extensions": nullable_closed_extensions_schema()
        },
        "required": ["outcome", "summary", "findings", "extensions"],
        "additionalProperties": false
    })
}

fn format_revision_context(revision_context: Option<&RevisionContext>) -> String {
    revision_context
        .map(|context| {
            serde_json::to_string_pretty(&context.payload).unwrap_or_else(|_| "{}".into())
        })
        .unwrap_or_else(|| "none".into())
}

pub(super) fn commit_subject(title: &str, step_id: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        format!("Ingot {step_id}")
    } else {
        format!("Ingot: {title}")
    }
}

pub(super) fn non_empty_message(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn template_digest(template: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(template.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_and_report_schemas_require_every_declared_property() {
        assert_schema_requires_all_properties(&commit_summary_schema());
        assert_schema_requires_all_properties(&validation_report_schema());
        assert_schema_requires_all_properties(&review_report_schema());
        assert_schema_requires_all_properties(&finding_report_schema());
    }

    #[test]
    fn nullable_fields_remain_present_in_required_schema_contracts() {
        let commit_schema = commit_summary_schema();
        assert_eq!(
            schema_property_type(&commit_schema, "validation"),
            Some(serde_json::json!(["string", "null"]))
        );

        let validation_schema = validation_report_schema();
        assert_eq!(
            schema_property(&validation_schema, "extensions"),
            Some(nullable_closed_extensions_schema())
        );
    }

    fn assert_schema_requires_all_properties(schema: &serde_json::Value) {
        let properties = schema["properties"]
            .as_object()
            .expect("schema properties object");
        let required = schema["required"]
            .as_array()
            .expect("schema required array")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let property_names = properties
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(required, property_names);
    }

    fn schema_property_type(
        schema: &serde_json::Value,
        property: &str,
    ) -> Option<serde_json::Value> {
        schema_property(schema, property).and_then(|value| value.get("type").cloned())
    }

    fn schema_property(schema: &serde_json::Value, property: &str) -> Option<serde_json::Value> {
        schema
            .get("properties")
            .and_then(|value| value.get(property))
            .cloned()
    }
}
