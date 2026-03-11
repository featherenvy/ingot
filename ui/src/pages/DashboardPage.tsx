import { useQuery } from '@tanstack/react-query'
import { useParams } from 'react-router'
import { itemsQuery } from '../api/queries'
import type { BoardStatus } from '../types/domain'

export default function DashboardPage() {
  const { projectId } = useParams<{ projectId: string }>()
  const { data: items, isLoading } = useQuery(itemsQuery(projectId!))

  if (isLoading) return <p>Loading...</p>

  const counts: Record<BoardStatus, number> = { INBOX: 0, WORKING: 0, APPROVAL: 0, DONE: 0 }
  for (const item of items ?? []) {
    const bs = (item as unknown as Record<string, string>).board_status as BoardStatus | undefined
    counts[bs ?? 'INBOX']++
  }

  return (
    <div>
      <h2>Dashboard</h2>
      <div style={{ display: 'flex', gap: '1rem' }}>
        {(['INBOX', 'WORKING', 'APPROVAL', 'DONE'] as const).map((col) => (
          <div
            key={col}
            style={{
              padding: '1rem',
              border: '1px solid #e5e5e5',
              borderRadius: 4,
              minWidth: 100,
              textAlign: 'center',
            }}
          >
            <div style={{ fontSize: '1.5rem', fontWeight: 'bold' }}>{counts[col]}</div>
            <div style={{ fontSize: '0.8rem', color: '#666' }}>{col}</div>
          </div>
        ))}
      </div>
    </div>
  )
}
