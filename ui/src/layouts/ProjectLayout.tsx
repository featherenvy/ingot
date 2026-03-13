import { useEffect } from 'react'
import { Outlet } from 'react-router'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { useProjectsStore } from '../stores/projects'

export default function ProjectLayout(): React.JSX.Element {
  const projectId = useRequiredProjectId()
  const setActive = useProjectsStore((s) => s.setActive)

  useEffect(() => {
    setActive(projectId)
    return () => setActive(null)
  }, [projectId, setActive])

  return <Outlet />
}
