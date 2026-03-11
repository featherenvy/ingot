import { useParams } from 'react-router'

export default function ConfigPage() {
  const { projectId } = useParams<{ projectId: string }>()

  return (
    <div>
      <h2>Config</h2>
      <p style={{ color: '#888' }}>Configuration for project {projectId}</p>
    </div>
  )
}
