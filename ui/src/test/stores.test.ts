import { useProjectsStore } from '../stores/projects'

describe('projects store', () => {
  beforeEach(() => {
    useProjectsStore.setState({ activeProjectId: null })
  })

  it('starts with no active project', () => {
    expect(useProjectsStore.getState().activeProjectId).toBeNull()
  })

  it('sets active project', () => {
    useProjectsStore.getState().setActive('prj_123')
    expect(useProjectsStore.getState().activeProjectId).toBe('prj_123')
  })

  it('clears active project', () => {
    useProjectsStore.getState().setActive('prj_123')
    useProjectsStore.getState().setActive(null)
    expect(useProjectsStore.getState().activeProjectId).toBeNull()
  })
})
