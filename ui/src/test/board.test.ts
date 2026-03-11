import type { BoardStatus, Item } from '../types/domain'

/** Derive board columns from a list of items with inline board_status. */
function deriveColumns(items: Array<Item & { board_status?: BoardStatus }>) {
  const cols: Record<BoardStatus, Item[]> = { INBOX: [], WORKING: [], APPROVAL: [], DONE: [] }
  for (const item of items) {
    const col = item.board_status ?? 'INBOX'
    cols[col].push(item)
  }
  return cols
}

function makeItem(overrides: Partial<Item> & { board_status?: BoardStatus }): Item & { board_status?: BoardStatus } {
  return {
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
    current_revision_id: 'rev_1',
    priority: 'major',
    labels: [],
    operator_notes: null,
    created_at: '2026-03-11T00:00:00Z',
    updated_at: '2026-03-11T00:00:00Z',
    closed_at: null,
    ...overrides,
  }
}

describe('board column derivation', () => {
  it('places items without board_status in INBOX', () => {
    const cols = deriveColumns([makeItem({ id: 'itm_1' })])
    expect(cols.INBOX).toHaveLength(1)
    expect(cols.WORKING).toHaveLength(0)
  })

  it('groups items by board_status', () => {
    const cols = deriveColumns([
      makeItem({ id: 'itm_1', board_status: 'INBOX' }),
      makeItem({ id: 'itm_2', board_status: 'WORKING' }),
      makeItem({ id: 'itm_3', board_status: 'WORKING' }),
      makeItem({ id: 'itm_4', board_status: 'APPROVAL' }),
      makeItem({
        id: 'itm_5',
        board_status: 'DONE',
        lifecycle_state: 'done',
        done_reason: 'completed',
        resolution_source: 'approval_command',
        closed_at: '2026-03-11T12:00:00Z',
      }),
    ])
    expect(cols.INBOX).toHaveLength(1)
    expect(cols.WORKING).toHaveLength(2)
    expect(cols.APPROVAL).toHaveLength(1)
    expect(cols.DONE).toHaveLength(1)
  })

  it('returns empty columns when no items', () => {
    const cols = deriveColumns([])
    expect(cols.INBOX).toHaveLength(0)
    expect(cols.WORKING).toHaveLength(0)
    expect(cols.APPROVAL).toHaveLength(0)
    expect(cols.DONE).toHaveLength(0)
  })
})
