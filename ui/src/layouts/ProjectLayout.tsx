import { useQuery } from '@tanstack/react-query'
import { useEffect } from 'react'
import { NavLink, Outlet, useParams } from 'react-router'
import { projectsQuery } from '../api/queries'
import { useProjectsStore } from '../stores/projects'

export default function ProjectLayout() {
  const { projectId } = useParams<{ projectId: string }>()
  const { data: projects } = useQuery(projectsQuery())
  const project = projects?.find((p) => p.id === projectId)
  const setActive = useProjectsStore((s) => s.setActive)

  // Keep the store in sync with the route param so the WS handler
  // knows which project's queries to invalidate.
  useEffect(() => {
    setActive(projectId ?? null)
    return () => setActive(null)
  }, [projectId, setActive])

  return (
    <div>
      <nav
        style={{
          display: 'flex',
          gap: '1rem',
          marginBottom: '1.5rem',
          borderBottom: '1px solid #e5e5e5',
          paddingBottom: '0.5rem',
        }}
      >
        <span style={{ fontWeight: 'bold', color: project?.color ?? '#000' }}>{project?.name ?? projectId}</span>
        <NavLink to={`/projects/${projectId}`} end style={linkStyle}>
          Dashboard
        </NavLink>
        <NavLink to={`/projects/${projectId}/board`} style={linkStyle}>
          Board
        </NavLink>
        <NavLink to={`/projects/${projectId}/jobs`} style={linkStyle}>
          Jobs
        </NavLink>
        <NavLink to={`/projects/${projectId}/workspaces`} style={linkStyle}>
          Workspaces
        </NavLink>
        <NavLink to={`/projects/${projectId}/config`} style={linkStyle}>
          Config
        </NavLink>
      </nav>
      <Outlet />
    </div>
  )
}

function linkStyle({ isActive }: { isActive: boolean }) {
  return {
    textDecoration: 'none',
    color: isActive ? '#000' : '#666',
    fontWeight: isActive ? ('bold' as const) : ('normal' as const),
  }
}
