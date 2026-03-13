import { useParams } from 'react-router'

export function useRequiredRouteParam(name: string): string {
  const params = useParams()
  const value = params[name]

  if (!value) {
    throw new Error(`Missing required route param: ${name}`)
  }

  return value
}

export function useRequiredProjectId(): string {
  return useRequiredRouteParam('projectId')
}

export function useRequiredItemId(): string {
  return useRequiredRouteParam('itemId')
}
