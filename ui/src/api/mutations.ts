import { useMutation, useQueryClient } from '@tanstack/react-query'
import * as api from './client'
import { queryKeys } from './queries'

export function useCreateItem(projectId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (data: Parameters<typeof api.createItem>[1]) => api.createItem(projectId, data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
    },
  })
}

export function useDeferItem(projectId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (itemId: string) => api.deferItem(projectId, itemId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
    },
  })
}

export function useResumeItem(projectId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (itemId: string) => api.resumeItem(projectId, itemId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
    },
  })
}

export function useDismissItem(projectId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (itemId: string) => api.dismissItem(projectId, itemId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
    },
  })
}

export function useApproveItem(projectId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (itemId: string) => api.approveItem(projectId, itemId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
    },
  })
}

export function useRejectItem(projectId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (itemId: string) => api.rejectItem(projectId, itemId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
    },
  })
}

export function useDispatchJob(projectId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (itemId: string) => api.dispatchJob(projectId, itemId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      qc.invalidateQueries({ queryKey: queryKeys.jobs(projectId) })
    },
  })
}

export function useCancelJob(projectId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ itemId, jobId }: { itemId: string; jobId: string }) => api.cancelJob(projectId, itemId, jobId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      qc.invalidateQueries({ queryKey: queryKeys.jobs(projectId) })
    },
  })
}

export function usePrepareConvergence(projectId: string) {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (itemId: string) => api.prepareConvergence(projectId, itemId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.items(projectId) })
      qc.invalidateQueries({ queryKey: queryKeys.convergences(projectId) })
    },
  })
}
