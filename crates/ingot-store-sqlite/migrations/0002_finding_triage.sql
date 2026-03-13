PRAGMA foreign_keys = OFF;

CREATE TABLE findings_new (
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
    paths TEXT NOT NULL DEFAULT '[]',
    evidence TEXT NOT NULL DEFAULT '[]',
    triage_state TEXT NOT NULL DEFAULT 'untriaged' CHECK (
        triage_state IN (
            'untriaged',
            'fix_now',
            'wont_fix',
            'backlog',
            'duplicate',
            'dismissed_invalid',
            'needs_investigation'
        )
    ),
    linked_item_id TEXT REFERENCES items(id),
    triage_note TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    triaged_at TEXT,

    CHECK (NOT (source_subject_kind = 'integrated' AND source_subject_base_commit_oid IS NULL)),
    CHECK (NOT (source_subject_kind = 'candidate' AND source_subject_head_commit_oid IS NULL)),
    CHECK (NOT (triage_state = 'untriaged' AND linked_item_id IS NOT NULL)),
    CHECK (NOT (triage_state = 'untriaged' AND triage_note IS NOT NULL)),
    CHECK (NOT (triage_state = 'untriaged' AND triaged_at IS NOT NULL)),
    CHECK (NOT (triage_state = 'fix_now' AND linked_item_id IS NOT NULL)),
    CHECK (NOT (triage_state = 'fix_now' AND triage_note IS NOT NULL)),
    CHECK (NOT (triage_state = 'fix_now' AND triaged_at IS NULL)),
    CHECK (NOT (triage_state = 'wont_fix' AND linked_item_id IS NOT NULL)),
    CHECK (NOT (triage_state = 'wont_fix' AND triage_note IS NULL)),
    CHECK (NOT (triage_state = 'wont_fix' AND triaged_at IS NULL)),
    CHECK (NOT (triage_state = 'backlog' AND linked_item_id IS NULL)),
    CHECK (NOT (triage_state = 'backlog' AND triaged_at IS NULL)),
    CHECK (NOT (triage_state = 'duplicate' AND linked_item_id IS NULL)),
    CHECK (NOT (triage_state = 'duplicate' AND triaged_at IS NULL)),
    CHECK (NOT (triage_state = 'dismissed_invalid' AND linked_item_id IS NOT NULL)),
    CHECK (NOT (triage_state = 'dismissed_invalid' AND triage_note IS NULL)),
    CHECK (NOT (triage_state = 'dismissed_invalid' AND triaged_at IS NULL)),
    CHECK (NOT (triage_state = 'needs_investigation' AND linked_item_id IS NOT NULL)),
    CHECK (NOT (triage_state = 'needs_investigation' AND triage_note IS NULL)),
    CHECK (NOT (triage_state = 'needs_investigation' AND triaged_at IS NULL))
);

INSERT INTO findings_new (
    id,
    project_id,
    source_item_id,
    source_item_revision_id,
    source_job_id,
    source_step_id,
    source_report_schema_version,
    source_finding_key,
    source_subject_kind,
    source_subject_base_commit_oid,
    source_subject_head_commit_oid,
    code,
    severity,
    summary,
    paths,
    evidence,
    triage_state,
    linked_item_id,
    triage_note,
    created_at,
    triaged_at
)
SELECT
    id,
    project_id,
    source_item_id,
    source_item_revision_id,
    source_job_id,
    source_step_id,
    source_report_schema_version,
    source_finding_key,
    source_subject_kind,
    source_subject_base_commit_oid,
    source_subject_head_commit_oid,
    code,
    severity,
    summary,
    paths,
    evidence,
    CASE triage_state
        WHEN 'promoted' THEN 'backlog'
        WHEN 'dismissed' THEN 'dismissed_invalid'
        ELSE triage_state
    END,
    promoted_item_id,
    dismissal_reason,
    created_at,
    triaged_at
FROM findings;

DROP TABLE findings;
ALTER TABLE findings_new RENAME TO findings;

CREATE INDEX idx_findings_item ON findings(source_item_id);
CREATE INDEX idx_findings_revision ON findings(source_item_revision_id);
CREATE INDEX idx_findings_job ON findings(source_job_id);
CREATE INDEX idx_findings_triage ON findings(triage_state);
CREATE INDEX idx_findings_linked_item ON findings(linked_item_id);

CREATE UNIQUE INDEX idx_findings_source_key
    ON findings(source_job_id, source_finding_key);

PRAGMA foreign_keys = ON;
