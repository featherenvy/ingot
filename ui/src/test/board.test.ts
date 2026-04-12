import { countItemSummariesByBoardStatus, groupItemSummariesByBoardStatus } from '../itemSummaries'
import type { Evaluation, Item, ItemSummary } from '../types/domain'

function makeItem(overrides: Partial<Item> = {}): Item {
  return {
    id: 'itm_1',
    sort_key: '2026-03-11T00:00:00Z#itm_1',
    project_id: 'prj_1',
    classification: 'change',
    workflow_version: 'delivery:v1',
    lifecycle_state: 'open',
    parking_state: 'active',
    approval_state: 'not_requested',
    escalation_state: 'none',
    current_revision_id: 'rev_1',
    origin_kind: 'manual',
    priority: 'major',
    labels: [],
    operator_notes: null,
    created_at: '2026-03-11T00:00:00Z',
    updated_at: '2026-03-11T00:00:00Z',
    ...overrides,
  }
}

function makeEvaluation(overrides: Partial<Evaluation> = {}): Evaluation {
  return {
    board_status: 'INBOX',
    attention_badges: [],
    current_step_id: null,
    current_phase_kind: null,
    phase_status: 'new',
    next_recommended_action: 'dispatch',
    dispatchable_step_id: 'author_initial',
    auxiliary_dispatchable_step_ids: [],
    allowed_actions: ['dispatch'],
    terminal_readiness: false,
    diagnostics: [],
    ...overrides,
  }
}

function makeItemSummary(item: Item, evaluation: Evaluation, title = 'Test item'): ItemSummary {
  return {
    item,
    title,
    evaluation,
    finalization: {
      phase: 'none',
      checkout_adoption_state: null,
      checkout_adoption_message: null,
      final_target_commit_oid: null,
    },
    queue: {
      state: null,
      position: null,
      lane_owner_item_id: null,
      lane_target_ref: null,
    },
  }
}

describe('board column derivation', () => {
  it('groups item summaries by evaluation.board_status', () => {
    const cols = groupItemSummariesByBoardStatus([
      makeItemSummary(makeItem({ id: 'itm_1' }), makeEvaluation({ board_status: 'INBOX' })),
      makeItemSummary(makeItem({ id: 'itm_2' }), makeEvaluation({ board_status: 'WORKING' })),
      makeItemSummary(makeItem({ id: 'itm_3' }), makeEvaluation({ board_status: 'WORKING' })),
      makeItemSummary(makeItem({ id: 'itm_4' }), makeEvaluation({ board_status: 'APPROVAL' })),
      makeItemSummary(
        makeItem({
          id: 'itm_5',
          lifecycle_state: 'done',
          done_reason: 'completed',
          resolution_source: 'approval_command',
          closed_at: '2026-03-11T12:00:00Z',
        }),
        makeEvaluation({ board_status: 'DONE', phase_status: 'done', next_recommended_action: 'none' }),
      ),
    ])

    expect(cols.INBOX).toHaveLength(1)
    expect(cols.INBOX).toHaveLength(1)
    expect(cols.WORKING).toHaveLength(2)
    expect(cols.APPROVAL).toHaveLength(1)
    expect(cols.DONE).toHaveLength(1)
  })

  it('counts item summaries by evaluation.board_status', () => {
    const counts = countItemSummariesByBoardStatus([
      makeItemSummary(makeItem({ id: 'itm_1' }), makeEvaluation({ board_status: 'INBOX' })),
      makeItemSummary(makeItem({ id: 'itm_2' }), makeEvaluation({ board_status: 'DONE', phase_status: 'done' })),
      makeItemSummary(makeItem({ id: 'itm_3' }), makeEvaluation({ board_status: 'DONE', phase_status: 'done' })),
    ])

    expect(counts.INBOX).toBe(1)
    expect(counts.WORKING).toBe(0)
    expect(counts.APPROVAL).toBe(0)
    expect(counts.DONE).toBe(2)
  })

  it('returns empty columns when no items', () => {
    const cols = groupItemSummariesByBoardStatus([])
    expect(cols.INBOX).toHaveLength(0)
    expect(cols.WORKING).toHaveLength(0)
    expect(cols.APPROVAL).toHaveLength(0)
    expect(cols.DONE).toHaveLength(0)
  })
})
