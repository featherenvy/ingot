-- Ingot v1 initial schema

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    default_branch TEXT NOT NULL DEFAULT 'main',
    color TEXT NOT NULL DEFAULT '#6366f1',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS agents (
    id TEXT PRIMARY KEY NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    adapter_kind TEXT NOT NULL CHECK (adapter_kind IN ('claude_code', 'codex')),
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    cli_path TEXT NOT NULL,
    capabilities TEXT NOT NULL DEFAULT '[]', -- JSON array
    health_check TEXT,
    status TEXT NOT NULL DEFAULT 'unavailable' CHECK (status IN ('available', 'unavailable', 'probing'))
);

CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    classification TEXT NOT NULL CHECK (classification IN ('change', 'bug')),
    workflow_version TEXT NOT NULL,
    lifecycle_state TEXT NOT NULL DEFAULT 'open' CHECK (lifecycle_state IN ('open', 'done')),
    parking_state TEXT NOT NULL DEFAULT 'active' CHECK (parking_state IN ('active', 'deferred')),
    done_reason TEXT CHECK (done_reason IN ('completed', 'dismissed', 'invalidated')),
    resolution_source TEXT CHECK (resolution_source IN ('system_command', 'approval_command', 'manual_command')),
    approval_state TEXT NOT NULL DEFAULT 'not_requested' CHECK (approval_state IN ('not_required', 'not_requested', 'pending', 'approved')),
    escalation_state TEXT NOT NULL DEFAULT 'none' CHECK (escalation_state IN ('none', 'operator_required')),
    escalation_reason TEXT CHECK (escalation_reason IN ('candidate_rework_budget_exhausted', 'integration_rework_budget_exhausted', 'convergence_conflict', 'step_failed', 'protocol_violation', 'manual_decision_required', 'other')),
    current_revision_id TEXT NOT NULL,
    origin_kind TEXT NOT NULL DEFAULT 'manual' CHECK (origin_kind IN ('manual', 'promoted_finding')),
    origin_finding_id TEXT REFERENCES findings(id),
    priority TEXT NOT NULL DEFAULT 'major' CHECK (priority IN ('critical', 'major', 'minor')),
    labels TEXT NOT NULL DEFAULT '[]', -- JSON array
    operator_notes TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    closed_at TEXT,

    CHECK (NOT (lifecycle_state = 'done' AND parking_state = 'deferred')),
    CHECK (NOT (approval_state = 'pending' AND parking_state = 'deferred')),
    CHECK (NOT (escalation_state = 'operator_required' AND lifecycle_state = 'done')),
    CHECK (NOT (lifecycle_state = 'done' AND done_reason IS NULL)),
    CHECK (NOT (lifecycle_state = 'done' AND resolution_source IS NULL)),
    CHECK (NOT (lifecycle_state = 'done' AND closed_at IS NULL)),
    CHECK (NOT (lifecycle_state = 'open' AND approval_state = 'approved')),
    CHECK (NOT (escalation_state = 'none' AND escalation_reason IS NOT NULL)),
    CHECK (NOT (escalation_state = 'operator_required' AND escalation_reason IS NULL)),
    CHECK (NOT (origin_kind = 'manual' AND origin_finding_id IS NOT NULL)),
    CHECK (NOT (origin_kind = 'promoted_finding' AND origin_finding_id IS NULL))
);

CREATE INDEX idx_items_project ON items(project_id);
CREATE INDEX idx_items_lifecycle ON items(lifecycle_state);
CREATE UNIQUE INDEX idx_items_origin_finding
    ON items(origin_finding_id)
    WHERE origin_finding_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS item_revisions (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL REFERENCES items(id),
    revision_no INTEGER NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    acceptance_criteria TEXT NOT NULL,
    target_ref TEXT NOT NULL,
    approval_policy TEXT NOT NULL CHECK (approval_policy IN ('required', 'not_required')),
    policy_snapshot TEXT NOT NULL DEFAULT '{}', -- JSON
    template_map_snapshot TEXT NOT NULL DEFAULT '{}', -- JSON
    seed_commit_oid TEXT,
    seed_target_commit_oid TEXT,
    supersedes_revision_id TEXT REFERENCES item_revisions(id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    UNIQUE (item_id, revision_no)
);

CREATE INDEX idx_revisions_item ON item_revisions(item_id);

CREATE TABLE IF NOT EXISTS revision_contexts (
    item_revision_id TEXT PRIMARY KEY NOT NULL REFERENCES item_revisions(id),
    schema_version TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}', -- JSON
    updated_from_job_id TEXT,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    kind TEXT NOT NULL CHECK (kind IN ('authoring', 'review', 'integration')),
    strategy TEXT NOT NULL DEFAULT 'worktree' CHECK (strategy IN ('worktree')),
    path TEXT NOT NULL,
    created_for_revision_id TEXT REFERENCES item_revisions(id),
    parent_workspace_id TEXT REFERENCES workspaces(id),
    target_ref TEXT,
    workspace_ref TEXT,
    base_commit_oid TEXT,
    head_commit_oid TEXT,
    retention_policy TEXT NOT NULL DEFAULT 'ephemeral' CHECK (retention_policy IN ('ephemeral', 'retain_until_debug', 'persistent')),
    status TEXT NOT NULL DEFAULT 'provisioning' CHECK (status IN ('provisioning', 'ready', 'busy', 'stale', 'retained_for_debug', 'abandoned', 'error', 'removing')),
    current_job_id TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_workspaces_project ON workspaces(project_id);
CREATE INDEX idx_workspaces_revision ON workspaces(created_for_revision_id);

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    item_id TEXT NOT NULL REFERENCES items(id),
    item_revision_id TEXT NOT NULL REFERENCES item_revisions(id),
    step_id TEXT NOT NULL,
    semantic_attempt_no INTEGER NOT NULL DEFAULT 1,
    retry_no INTEGER NOT NULL DEFAULT 0,
    supersedes_job_id TEXT REFERENCES jobs(id),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'assigned', 'running', 'completed', 'failed', 'cancelled', 'expired', 'superseded')),
    outcome_class TEXT CHECK (outcome_class IN ('clean', 'findings', 'transient_failure', 'terminal_failure', 'protocol_violation', 'cancelled')),
    phase_kind TEXT NOT NULL CHECK (phase_kind IN ('author', 'validate', 'review', 'investigate', 'system')),
    workspace_id TEXT REFERENCES workspaces(id),
    workspace_kind TEXT NOT NULL CHECK (workspace_kind IN ('authoring', 'review', 'integration')),
    execution_permission TEXT NOT NULL CHECK (execution_permission IN ('may_mutate', 'must_not_mutate', 'daemon_only')),
    context_policy TEXT NOT NULL CHECK (context_policy IN ('fresh', 'resume_context', 'none')),
    phase_template_slug TEXT NOT NULL,
    phase_template_digest TEXT,
    prompt_snapshot TEXT,
    job_input_kind TEXT NOT NULL DEFAULT 'none' CHECK (job_input_kind IN ('none', 'authoring_head', 'candidate_subject', 'integrated_subject')),
    input_base_commit_oid TEXT,
    input_head_commit_oid TEXT,
    output_artifact_kind TEXT NOT NULL CHECK (output_artifact_kind IN ('commit', 'review_report', 'validation_report', 'finding_report', 'none')),
    output_commit_oid TEXT,
    result_schema_version TEXT,
    result_payload TEXT, -- JSON
    agent_id TEXT REFERENCES agents(id),
    process_pid INTEGER,
    lease_owner_id TEXT,
    heartbeat_at TEXT,
    lease_expires_at TEXT,
    error_code TEXT,
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    started_at TEXT,
    ended_at TEXT,

    CHECK (NOT (job_input_kind = 'none' AND input_base_commit_oid IS NOT NULL)),
    CHECK (NOT (job_input_kind = 'none' AND input_head_commit_oid IS NOT NULL)),
    CHECK (NOT (job_input_kind = 'authoring_head' AND input_base_commit_oid IS NOT NULL)),
    CHECK (NOT (job_input_kind = 'authoring_head' AND input_head_commit_oid IS NULL)),
    CHECK (NOT (job_input_kind IN ('candidate_subject', 'integrated_subject') AND input_base_commit_oid IS NULL)),
    CHECK (NOT (job_input_kind IN ('candidate_subject', 'integrated_subject') AND input_head_commit_oid IS NULL))
);

CREATE INDEX idx_jobs_project ON jobs(project_id);
CREATE INDEX idx_jobs_item ON jobs(item_id);
CREATE INDEX idx_jobs_revision ON jobs(item_revision_id);
CREATE INDEX idx_jobs_status ON jobs(status);

CREATE UNIQUE INDEX idx_jobs_active_per_revision
    ON jobs(item_revision_id)
    WHERE status IN ('queued', 'assigned', 'running');

CREATE UNIQUE INDEX idx_jobs_step_attempt_retry
    ON jobs(item_revision_id, step_id, semantic_attempt_no, retry_no);

CREATE TABLE IF NOT EXISTS convergences (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    item_id TEXT NOT NULL REFERENCES items(id),
    item_revision_id TEXT NOT NULL REFERENCES item_revisions(id),
    source_workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    integration_workspace_id TEXT REFERENCES workspaces(id),
    source_head_commit_oid TEXT NOT NULL,
    target_ref TEXT NOT NULL,
    strategy TEXT NOT NULL DEFAULT 'rebase_then_fast_forward' CHECK (strategy IN ('rebase_then_fast_forward')),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'conflicted', 'prepared', 'finalized', 'failed', 'cancelled')),
    input_target_commit_oid TEXT,
    prepared_commit_oid TEXT,
    final_target_commit_oid TEXT,
    conflict_summary TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT,
    CHECK (
        status != 'running'
        OR (integration_workspace_id IS NOT NULL AND input_target_commit_oid IS NOT NULL)
    ),
    CHECK (
        status != 'conflicted'
        OR (
            integration_workspace_id IS NOT NULL
            AND input_target_commit_oid IS NOT NULL
            AND conflict_summary IS NOT NULL
            AND completed_at IS NOT NULL
        )
    ),
    CHECK (
        status != 'prepared'
        OR (
            integration_workspace_id IS NOT NULL
            AND input_target_commit_oid IS NOT NULL
            AND prepared_commit_oid IS NOT NULL
        )
    ),
    CHECK (
        status != 'finalized'
        OR (
            input_target_commit_oid IS NOT NULL
            AND prepared_commit_oid IS NOT NULL
            AND final_target_commit_oid IS NOT NULL
            AND completed_at IS NOT NULL
        )
    ),
    CHECK (status != 'failed' OR completed_at IS NOT NULL),
    CHECK (status != 'cancelled' OR completed_at IS NOT NULL)
);

CREATE INDEX idx_convergences_revision ON convergences(item_revision_id);

CREATE UNIQUE INDEX idx_convergences_active_per_revision
    ON convergences(item_revision_id)
    WHERE status IN ('queued', 'running', 'prepared');

CREATE TABLE IF NOT EXISTS findings (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    source_item_id TEXT NOT NULL REFERENCES items(id),
    source_item_revision_id TEXT NOT NULL REFERENCES item_revisions(id),
    source_job_id TEXT NOT NULL REFERENCES jobs(id),
    source_step_id TEXT NOT NULL,
    source_report_schema_version TEXT NOT NULL,
    source_finding_key TEXT NOT NULL,
    source_subject_kind TEXT NOT NULL CHECK (source_subject_kind IN ('candidate', 'integrated')),
    source_subject_base_commit_oid TEXT,
    source_subject_head_commit_oid TEXT NOT NULL,
    code TEXT NOT NULL,
    severity TEXT NOT NULL CHECK (severity IN ('low', 'medium', 'high', 'critical')),
    summary TEXT NOT NULL,
    paths TEXT NOT NULL DEFAULT '[]', -- JSON array
    evidence TEXT NOT NULL DEFAULT '[]', -- JSON array
    triage_state TEXT NOT NULL DEFAULT 'untriaged' CHECK (triage_state IN ('untriaged', 'promoted', 'dismissed')),
    promoted_item_id TEXT REFERENCES items(id),
    dismissal_reason TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    triaged_at TEXT,

    CHECK (NOT (source_subject_kind = 'integrated' AND source_subject_base_commit_oid IS NULL)),
    CHECK (NOT (source_subject_kind = 'candidate' AND source_subject_head_commit_oid IS NULL)),
    CHECK (NOT (triage_state = 'untriaged' AND promoted_item_id IS NOT NULL)),
    CHECK (NOT (triage_state = 'untriaged' AND dismissal_reason IS NOT NULL)),
    CHECK (NOT (triage_state = 'untriaged' AND triaged_at IS NOT NULL)),
    CHECK (NOT (triage_state = 'promoted' AND promoted_item_id IS NULL)),
    CHECK (NOT (triage_state = 'promoted' AND dismissal_reason IS NOT NULL)),
    CHECK (NOT (triage_state = 'promoted' AND triaged_at IS NULL)),
    CHECK (NOT (triage_state = 'dismissed' AND dismissal_reason IS NULL)),
    CHECK (NOT (triage_state = 'dismissed' AND promoted_item_id IS NOT NULL)),
    CHECK (NOT (triage_state = 'dismissed' AND triaged_at IS NULL))
);

CREATE INDEX idx_findings_item ON findings(source_item_id);
CREATE INDEX idx_findings_revision ON findings(source_item_revision_id);
CREATE INDEX idx_findings_job ON findings(source_job_id);
CREATE INDEX idx_findings_triage ON findings(triage_state);

CREATE UNIQUE INDEX idx_findings_source_key
    ON findings(source_job_id, source_finding_key);

CREATE UNIQUE INDEX idx_findings_promoted_item
    ON findings(promoted_item_id)
    WHERE promoted_item_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS git_operations (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    operation_kind TEXT NOT NULL CHECK (operation_kind IN ('create_job_commit', 'prepare_convergence_commit', 'finalize_target_ref', 'create_investigation_ref', 'remove_investigation_ref', 'reset_workspace', 'remove_workspace_ref')),
    entity_type TEXT NOT NULL CHECK (entity_type IN ('job', 'convergence', 'workspace', 'item_revision')),
    entity_id TEXT NOT NULL,
    workspace_id TEXT REFERENCES workspaces(id),
    ref_name TEXT,
    expected_old_oid TEXT,
    new_oid TEXT,
    commit_oid TEXT,
    status TEXT NOT NULL DEFAULT 'planned' CHECK (status IN ('planned', 'applied', 'reconciled', 'failed')),
    metadata TEXT, -- JSON
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT
);

CREATE INDEX idx_git_ops_status ON git_operations(status);

CREATE TABLE IF NOT EXISTS activity (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    event_type TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    payload TEXT NOT NULL DEFAULT '{}', -- JSON
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_activity_project ON activity(project_id);
CREATE INDEX idx_activity_created ON activity(created_at);
