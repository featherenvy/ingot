import type { ItemDetail, ItemSummary } from '../types/domain'

describe('domain contract typing', () => {
  it('models list responses as item summaries with nested evaluation data', () => {
    const summary: ItemSummary = {
      title: 'Fix critical bug',
      item: {
        id: 'itm_1',
        project_id: 'prj_1',
        classification: 'bug',
        workflow_version: 'delivery:v1',
        lifecycle_state: 'open',
        parking_state: 'active',
        done_reason: null,
        resolution_source: null,
        approval_state: 'pending',
        escalation_state: 'none',
        escalation_reason: null,
        current_revision_id: 'rev_1',
        origin_kind: 'promoted_finding',
        origin_finding_id: 'fnd_1',
        priority: 'critical',
        labels: ['backend'],
        operator_notes: 'Needs triage',
        created_at: '2026-03-11T00:00:00Z',
        updated_at: '2026-03-11T00:10:00Z',
        closed_at: null,
      },
      evaluation: {
        board_status: 'WORKING',
        attention_badges: ['escalated'],
        current_step_id: 'review_incremental_initial',
        current_phase_kind: 'investigate',
        phase_status: 'running',
        next_recommended_action: 'none',
        dispatchable_step_id: null,
        auxiliary_dispatchable_step_ids: ['investigate_item'],
        allowed_actions: ['cancel_job'],
        terminal_readiness: false,
        diagnostics: ['active investigation job'],
      },
    }

    expect(summary.item.origin_finding_id).toBe('fnd_1')
    expect(summary.evaluation.current_phase_kind).toBe('investigate')
    expect(summary.evaluation.auxiliary_dispatchable_step_ids).toEqual(['investigate_item'])
  })

  it('models item detail findings, revision context, and convergence responses', () => {
    const detail: ItemDetail = {
      item: {
        id: 'itm_1',
        project_id: 'prj_1',
        classification: 'change',
        workflow_version: 'delivery:v1',
        lifecycle_state: 'open',
        parking_state: 'active',
        done_reason: null,
        resolution_source: null,
        approval_state: 'not_requested',
        escalation_state: 'none',
        escalation_reason: null,
        current_revision_id: 'rev_2',
        origin_kind: 'manual',
        origin_finding_id: null,
        priority: 'major',
        labels: [],
        operator_notes: null,
        created_at: '2026-03-11T00:00:00Z',
        updated_at: '2026-03-11T00:10:00Z',
        closed_at: null,
      },
      current_revision: {
        id: 'rev_2',
        item_id: 'itm_1',
        revision_no: 2,
        title: 'Fix issue',
        description: 'Update the implementation',
        acceptance_criteria: 'All checks pass',
        target_ref: 'main',
        approval_policy: 'required',
        seed_commit_oid: 'abc123456789',
        supersedes_revision_id: 'rev_1',
        created_at: '2026-03-11T00:05:00Z',
      },
      evaluation: {
        board_status: 'APPROVAL',
        attention_badges: [],
        current_step_id: 'validate_integrated',
        current_phase_kind: 'system',
        phase_status: 'pending_approval',
        next_recommended_action: 'approval_approve',
        dispatchable_step_id: null,
        auxiliary_dispatchable_step_ids: [],
        allowed_actions: ['approval_approve', 'approval_reject'],
        terminal_readiness: false,
        diagnostics: [],
      },
      revision_history: [],
      jobs: [
        {
          id: 'job_1',
          project_id: 'prj_1',
          item_id: 'itm_1',
          item_revision_id: 'rev_2',
          step_id: 'investigate_item',
          status: 'completed',
          outcome_class: 'findings',
          phase_kind: 'investigate',
          workspace_id: null,
          created_at: '2026-03-11T00:06:00Z',
          started_at: '2026-03-11T00:06:10Z',
          ended_at: '2026-03-11T00:07:00Z',
        },
      ],
      findings: [
        {
          id: 'fnd_1',
          project_id: 'prj_1',
          source_item_id: 'itm_1',
          source_item_revision_id: 'rev_2',
          source_job_id: 'job_1',
          source_step_id: 'investigate_item',
          source_report_schema_version: 'finding_report:v1',
          source_finding_key: 'finding-1',
          source_subject_kind: 'candidate',
          source_subject_base_commit_oid: 'abc123456789',
          source_subject_head_commit_oid: 'def456789012',
          code: 'STYLE',
          severity: 'medium',
          summary: 'Refactor repeated logic',
          paths: ['src/lib.rs'],
          evidence: { message: 'Duplicate branch logic', line: 42 },
          triage_state: 'untriaged',
          linked_item_id: null,
          triage_note: null,
          created_at: '2026-03-11T00:07:00Z',
          triaged_at: null,
        },
      ],
      workspaces: [
        {
          id: 'wrk_1',
          kind: 'integration',
          status: 'ready',
          target_ref: 'main',
          workspace_ref: 'refs/workspaces/wrk_1',
          base_commit_oid: 'abc123456789',
          head_commit_oid: 'def456789012',
        },
      ],
      convergences: [
        {
          id: 'conv_1',
          status: 'prepared',
          input_target_commit_oid: 'abc123456789',
          prepared_commit_oid: 'def456789012',
          final_target_commit_oid: null,
          target_head_valid: true,
        },
      ],
      revision_context_summary: {
        updated_at: '2026-03-11T00:08:00Z',
        changed_paths: ['src/lib.rs'],
        latest_validation: {
          job_id: 'job_2',
          schema_version: 'validation_report:v1',
          outcome: 'clean',
          summary: 'Validation passed',
        },
        latest_review: null,
        accepted_result_refs: [
          {
            job_id: 'job_2',
            step_id: 'validate_integrated',
            schema_version: 'validation_report:v1',
            outcome: 'clean',
            summary: 'Validation passed',
          },
        ],
        operator_notes_excerpt: null,
      },
      diagnostics: [],
    }

    expect(detail.findings[0]?.evidence).toEqual({ message: 'Duplicate branch logic', line: 42 })
    expect(detail.revision_context_summary?.accepted_result_refs).toHaveLength(1)
    expect(detail.convergences[0]?.final_target_commit_oid).toBeNull()
  })
})
