-- Investigation workflow: add 'investigation' classification and 'investigation_report' artifact kind

PRAGMA foreign_keys = OFF;

-- ============================================================
-- 1. Items table rebuild: add 'investigation' to classification CHECK
-- ============================================================

CREATE TABLE items_new (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    classification TEXT NOT NULL CHECK (classification IN ('change', 'bug', 'investigation')),
    workflow_version TEXT NOT NULL,
    lifecycle_state TEXT NOT NULL DEFAULT 'open' CHECK (lifecycle_state IN ('open', 'done')),
    parking_state TEXT NOT NULL DEFAULT 'active' CHECK (parking_state IN ('active', 'deferred')),
    done_reason TEXT CHECK (done_reason IN ('completed', 'dismissed', 'invalidated')),
    resolution_source TEXT CHECK (resolution_source IN ('system_command', 'approval_command', 'manual_command')),
    approval_state TEXT NOT NULL DEFAULT 'not_requested' CHECK (
        approval_state IN ('not_required', 'not_requested', 'pending', 'approved')
    ),
    escalation_state TEXT NOT NULL DEFAULT 'none' CHECK (escalation_state IN ('none', 'operator_required')),
    escalation_reason TEXT CHECK (
        escalation_reason IN (
            'candidate_rework_budget_exhausted',
            'integration_rework_budget_exhausted',
            'convergence_conflict',
            'checkout_sync_blocked',
            'step_failed',
            'protocol_violation',
            'manual_decision_required',
            'other'
        )
    ),
    current_revision_id TEXT NOT NULL,
    origin_kind TEXT NOT NULL DEFAULT 'manual' CHECK (origin_kind IN ('manual', 'promoted_finding')),
    origin_finding_id TEXT REFERENCES findings(id),
    priority TEXT NOT NULL DEFAULT 'major' CHECK (priority IN ('critical', 'major', 'minor')),
    labels TEXT NOT NULL DEFAULT '[]',
    operator_notes TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    closed_at TEXT,
    sort_key TEXT NOT NULL DEFAULT '',

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

INSERT INTO items_new SELECT * FROM items;

DROP TABLE items;
ALTER TABLE items_new RENAME TO items;

CREATE INDEX idx_items_project ON items(project_id);
CREATE INDEX idx_items_lifecycle ON items(lifecycle_state);
CREATE UNIQUE INDEX idx_items_origin_finding
    ON items(origin_finding_id)
    WHERE origin_finding_id IS NOT NULL;

-- ============================================================
-- 2. Jobs table rebuild: add 'investigation_report' to output_artifact_kind CHECK
-- ============================================================

CREATE TABLE jobs_new (
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
    output_artifact_kind TEXT NOT NULL CHECK (output_artifact_kind IN ('commit', 'review_report', 'validation_report', 'finding_report', 'investigation_report', 'none')),
    output_commit_oid TEXT,
    result_schema_version TEXT,
    result_payload TEXT,
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

INSERT INTO jobs_new SELECT * FROM jobs;

DROP TABLE jobs;
ALTER TABLE jobs_new RENAME TO jobs;

CREATE INDEX idx_jobs_project ON jobs(project_id);
CREATE INDEX idx_jobs_item ON jobs(item_id);
CREATE INDEX idx_jobs_revision ON jobs(item_revision_id);
CREATE INDEX idx_jobs_status ON jobs(status);

CREATE UNIQUE INDEX idx_jobs_active_per_revision
    ON jobs(item_revision_id)
    WHERE status IN ('queued', 'assigned', 'running');

CREATE UNIQUE INDEX idx_jobs_step_attempt_retry
    ON jobs(item_revision_id, step_id, semantic_attempt_no, retry_no);

PRAGMA foreign_keys = ON;
