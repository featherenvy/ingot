import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect } from 'react'
import { Outlet } from 'react-router'
import { healthQuery } from '../api/queries'
import { useConnectionStore } from '../stores/connection'

export default function RootLayout() {
  const queryClient = useQueryClient()
  const { status: wsStatus, connect } = useConnectionStore()
  const { data: health } = useQuery(healthQuery())

  useEffect(() => {
    connect(queryClient)
  }, [connect, queryClient])

  return (
    <div style={{ fontFamily: 'system-ui' }}>
      <header
        style={{
          padding: '0.75rem 1.5rem',
          borderBottom: '1px solid #e5e5e5',
          display: 'flex',
          alignItems: 'center',
          gap: '1rem',
        }}
      >
        <strong>Ingot</strong>
        <span style={{ fontSize: '0.8rem', color: '#888' }}>
          daemon: {health ?? '...'} | ws: {wsStatus}
        </span>
      </header>
      <main style={{ padding: '1.5rem' }}>
        <Outlet />
      </main>
    </div>
  )
}
