import { toast } from 'sonner'
import { ApiError } from '../api/client'

export function getErrorMessage(error: unknown, fallback = 'Something went wrong.') {
  if (error instanceof ApiError && error.message) return error.message
  if (error instanceof Error && error.message) return error.message
  return fallback
}

export function showErrorToast(title: string, error: unknown, fallback?: string) {
  toast.error(title, {
    description: getErrorMessage(error, fallback),
  })
}
