import { useParams } from 'react-router'

export default function JobsPage() {
  const { projectId } = useParams<{ projectId: string }>()

  return (
    <div>
      <h2>Jobs</h2>
      <p style={{ color: '#888' }}>Execution queue for project {projectId}</p>
    </div>
  )
}
