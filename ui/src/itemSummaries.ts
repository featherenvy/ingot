import type { BoardStatus, ItemSummary } from './types/domain'

export const boardStatuses = ['INBOX', 'WORKING', 'APPROVAL', 'DONE'] as const satisfies readonly BoardStatus[]

export function createEmptyBoardCounts(): Record<BoardStatus, number> {
  return {
    INBOX: 0,
    WORKING: 0,
    APPROVAL: 0,
    DONE: 0,
  }
}

export function createEmptyBoardColumns(): Record<BoardStatus, ItemSummary[]> {
  return {
    INBOX: [],
    WORKING: [],
    APPROVAL: [],
    DONE: [],
  }
}

export function countItemSummariesByBoardStatus(items: ItemSummary[]): Record<BoardStatus, number> {
  const counts = createEmptyBoardCounts()

  for (const itemSummary of items) {
    counts[itemSummary.evaluation.board_status] += 1
  }

  return counts
}

export function groupItemSummariesByBoardStatus(items: ItemSummary[]): Record<BoardStatus, ItemSummary[]> {
  const columns = createEmptyBoardColumns()

  for (const itemSummary of items) {
    columns[itemSummary.evaluation.board_status].push(itemSummary)
  }

  return columns
}
