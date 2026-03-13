import type { ComponentProps, ReactNode } from 'react'
import { useState } from 'react'
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from './ui/alert-dialog'
import { Button } from './ui/button'

type ConfirmActionButtonProps = {
  title: string
  description: ReactNode
  triggerLabel: string
  confirmLabel: string
  pendingLabel: string
  onConfirm: () => void
  pending: boolean
  triggerVariant?: ComponentProps<typeof Button>['variant']
  triggerSize?: ComponentProps<typeof Button>['size']
}

export function ConfirmActionButton({
  title,
  description,
  triggerLabel,
  confirmLabel,
  pendingLabel,
  onConfirm,
  pending,
  triggerVariant = 'outline',
  triggerSize = 'sm',
}: ConfirmActionButtonProps): React.JSX.Element {
  const [open, setOpen] = useState(false)

  function handleOpenChange(nextOpen: boolean): void {
    if (pending) return
    setOpen(nextOpen)
  }

  function handleConfirm(): void {
    onConfirm()
    setOpen(false)
  }

  return (
    <AlertDialog open={open} onOpenChange={handleOpenChange}>
      <AlertDialogTrigger asChild>
        <Button type="button" size={triggerSize} variant={triggerVariant} disabled={pending}>
          {pending ? pendingLabel : triggerLabel}
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{description}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={pending}>Keep</AlertDialogCancel>
          <Button type="button" variant="destructive" onClick={handleConfirm} disabled={pending}>
            {pending ? pendingLabel : confirmLabel}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
