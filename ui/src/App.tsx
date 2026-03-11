import { BrowserRouter, Navigate, Route, Routes } from 'react-router'
import ProjectLayout from './layouts/ProjectLayout'
import RootLayout from './layouts/RootLayout'
import BoardPage from './pages/BoardPage'
import ConfigPage from './pages/ConfigPage'
import DashboardPage from './pages/DashboardPage'
import ItemDetailPage from './pages/ItemDetailPage'
import JobsPage from './pages/JobsPage'
import ProjectsPage from './pages/ProjectsPage'
import WorkspacesPage from './pages/WorkspacesPage'

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route element={<RootLayout />}>
          <Route index element={<ProjectsPage />} />
          <Route path="projects/:projectId" element={<ProjectLayout />}>
            <Route index element={<DashboardPage />} />
            <Route path="board" element={<BoardPage />} />
            <Route path="items/:itemId" element={<ItemDetailPage />} />
            <Route path="jobs" element={<JobsPage />} />
            <Route path="workspaces" element={<WorkspacesPage />} />
            <Route path="config" element={<ConfigPage />} />
          </Route>
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Routes>
    </BrowserRouter>
  )
}
