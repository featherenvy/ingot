import { create } from 'zustand'

/**
 * Client-only state for tracking which project the operator is viewing.
 * Set by ProjectLayout when the route param changes.
 * Read by the WS connection store for targeted cache invalidation.
 */
interface ProjectsState {
  activeProjectId: string | null
  setActive: (id: string | null) => void
}

export const useProjectsStore = create<ProjectsState>((set) => ({
  activeProjectId: null,
  setActive: (id) => set({ activeProjectId: id }),
}))
