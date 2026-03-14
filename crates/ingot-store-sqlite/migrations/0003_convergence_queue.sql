PRAGMA foreign_keys = OFF;

CREATE TABLE items_new (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    classification TEXT NOT NULL CHECK (classification IN ('change', 'bug')),
    workflow_version TEXT NOT NULL,
    lifecycle_state TEXT NOT NULL DEFAULT 'open' CHECK (lifecycle_state IN ('open', 'done')),
    parking_state TEXT NOT NULL DEFAULT 'active' CHECK (parking_state IN ('active', 'deferred')),
    done_reason TEXT CHECK (done_reason IN ('completed', 'dismissed', 'invalidated')),
    resolution_source TEXT CHECK (resolution_source IN ('system_command', 'approval_command', 'manual_command')),
    approval_state TEXT NOT NULL DEFAULT 'not_requested' CHECK (
        approval_state IN ('not_required', 'not_requested', 'pending', 'granted', 'approved')
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

    CHECK (NOT (lifecycle_state = 'done' AND parking_state = 'deferred')),
    CHECK (NOT (approval_state = 'pending' AND parking_state = 'deferred')),
    CHECK (NOT (approval_state = 'granted' AND parking_state = 'deferred')),
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

INSERT INTO items_new (
    id,
    project_id,
    classification,
    workflow_version,
    lifecycle_state,
    parking_state,
    done_reason,
    resolution_source,
    approval_state,
    escalation_state,
    escalation_reason,
    current_revision_id,
    origin_kind,
    origin_finding_id,
    priority,
    labels,
    operator_notes,
    created_at,
    updated_at,
    closed_at
)
SELECT
    id,
    project_id,
    classification,
    workflow_version,
    lifecycle_state,
    parking_state,
    done_reason,
    resolution_source,
    approval_state,
    escalation_state,
    escalation_reason,
    current_revision_id,
    origin_kind,
    origin_finding_id,
    priority,
    labels,
    operator_notes,
    created_at,
    updated_at,
    closed_at
FROM items;

DROP TABLE items;
ALTER TABLE items_new RENAME TO items;

CREATE INDEX idx_items_project ON items(project_id);
CREATE INDEX idx_items_lifecycle ON items(lifecycle_state);
CREATE UNIQUE INDEX idx_items_origin_finding
    ON items(origin_finding_id)
    WHERE origin_finding_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS convergence_queue_entries (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    item_id TEXT NOT NULL REFERENCES items(id),
    item_revision_id TEXT NOT NULL REFERENCES item_revisions(id),
    target_ref TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'head', 'released', 'cancelled')),
    head_acquired_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    released_at TEXT
);

CREATE INDEX idx_convergence_queue_item ON convergence_queue_entries(item_id);
CREATE INDEX idx_convergence_queue_revision ON convergence_queue_entries(item_revision_id);
CREATE INDEX idx_convergence_queue_lane ON convergence_queue_entries(project_id, target_ref, created_at, id);

CREATE UNIQUE INDEX idx_convergence_queue_active_revision
    ON convergence_queue_entries(item_revision_id)
    WHERE status IN ('queued', 'head');

CREATE UNIQUE INDEX idx_convergence_queue_head_per_lane
    ON convergence_queue_entries(project_id, target_ref)
    WHERE status = 'head';

PRAGMA foreign_keys = ON;
