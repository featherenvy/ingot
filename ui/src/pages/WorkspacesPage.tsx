import { useParams } from 'react-router'

export default function WorkspacesPage() {
  const { projectId } = useParams<{ projectId: string }>()

  return (
    <div>
      <h2>Workspaces</h2>
      <p style={{ color: '#888' }}>Workspace management for project {projectId}</p>
    </div>
  )
}
