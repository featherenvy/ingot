import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { Link, useParams } from 'react-router'
import { itemsQuery } from '../api/queries'
import type { BoardStatus, Item } from '../types/domain'

export default function BoardPage() {
  const { projectId } = useParams<{ projectId: string }>()
  const { data: items, isLoading } = useQuery(itemsQuery(projectId!))

  const columns = useMemo(() => {
    const cols: Record<BoardStatus, Item[]> = { INBOX: [], WORKING: [], APPROVAL: [], DONE: [] }
    for (const item of items ?? []) {
      const bs = (item as unknown as Record<string, string>).board_status as BoardStatus | undefined
      cols[bs ?? 'INBOX'].push(item)
    }
    return cols
  }, [items])

  if (isLoading) return <p>Loading...</p>

  return (
    <div>
      <h2>Board ({items?.length ?? 0} items)</h2>
      <div style={{ display: 'flex', gap: '1rem' }}>
        {(['INBOX', 'WORKING', 'APPROVAL', 'DONE'] as const).map((col) => (
          <div key={col} style={{ flex: 1, border: '1px solid #e5e5e5', borderRadius: 4, padding: '0.5rem' }}>
            <h3 style={{ fontSize: '0.85rem', color: '#666', margin: '0 0 0.5rem' }}>
              {col} ({columns[col].length})
            </h3>
            {columns[col].map((item) => (
              <Link
                key={item.id}
                to={`/projects/${projectId}/items/${item.id}`}
                style={{
                  display: 'block',
                  padding: '0.5rem',
                  marginBottom: '0.25rem',
                  background: '#f9f9f9',
                  borderRadius: 3,
                  textDecoration: 'none',
                  color: 'inherit',
                }}
              >
                <strong style={{ fontSize: '0.8rem' }}>{item.id}</strong>
                <div style={{ fontSize: '0.75rem', color: '#888' }}>
                  {item.priority} | {item.classification}
                </div>
              </Link>
            ))}
          </div>
        ))}
      </div>
    </div>
  )
}
