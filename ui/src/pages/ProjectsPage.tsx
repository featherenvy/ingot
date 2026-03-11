import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'
import { projectsQuery } from '../api/queries'

export default function ProjectsPage() {
  const { data: projects, isLoading } = useQuery(projectsQuery())

  if (isLoading) return <p>Loading projects...</p>

  return (
    <div>
      <h2>Projects</h2>
      {projects && projects.length > 0 ? (
        <ul style={{ listStyle: 'none', padding: 0 }}>
          {projects.map((p) => (
            <li key={p.id} style={{ marginBottom: '0.5rem' }}>
              <Link to={`/projects/${p.id}`} style={{ textDecoration: 'none' }}>
                <span
                  style={{
                    display: 'inline-block',
                    width: 12,
                    height: 12,
                    borderRadius: '50%',
                    background: p.color,
                    marginRight: 8,
                  }}
                />
                {p.name}
                <span style={{ color: '#888', marginLeft: 8, fontSize: '0.85rem' }}>{p.path}</span>
              </Link>
            </li>
          ))}
        </ul>
      ) : (
        <p>No projects registered.</p>
      )}
    </div>
  )
}
