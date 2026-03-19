import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { cn } from '@/lib/utils'
import { createDemoProject } from '../api/client'
import { demoCatalogQuery, queryKeys } from '../api/queries'
import { showErrorToast } from '../lib/toast'
import { Badge } from './ui/badge'
import { Button } from './ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/card'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from './ui/dialog'
import { Label } from './ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select'

type DemoProjectDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function DemoProjectDialog({ open, onOpenChange }: DemoProjectDialogProps): React.JSX.Element {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const { data } = useQuery(demoCatalogQuery())
  const templates = data?.templates ?? []

  const [selectedTemplate, setSelectedTemplate] = useState('mini-crm')
  const [selectedStack, setSelectedStack] = useState('express-react')

  const currentTemplate = templates.find((t) => t.slug === selectedTemplate)
  const stacks = currentTemplate?.stacks ?? []

  const mutation = useMutation({
    mutationFn: () => createDemoProject({ template: selectedTemplate, stack: selectedStack }),
    onSuccess: (result) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.projects() })
      handleOpenChange(false)
      toast.success(`Demo project created with ${result.items_created} items.`)
      navigate(`/projects/${result.project.id}`)
    },
    onError: (error) => {
      showErrorToast('Failed to create demo project.', error)
    },
  })

  function handleOpenChange(next: boolean) {
    onOpenChange(next)
    if (!next) {
      setSelectedTemplate('mini-crm')
      setSelectedStack('express-react')
      mutation.reset()
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Try a demo project</DialogTitle>
          <DialogDescription>
            Pick a template and tech stack. Items describe what to build; the README guides which frameworks to use.
          </DialogDescription>
        </DialogHeader>

        <div className="grid grid-cols-2 gap-3">
          {templates.map((t) => (
            <Card
              key={t.slug}
              size="sm"
              className={cn('cursor-pointer', selectedTemplate === t.slug && 'ring-2 ring-primary')}
              onClick={() => {
                setSelectedTemplate(t.slug)
                setSelectedStack(t.stacks[0]?.slug ?? '')
              }}
            >
              <CardHeader>
                <CardTitle>{t.name}</CardTitle>
                <CardDescription>{t.description}</CardDescription>
              </CardHeader>
              <CardContent>
                <Badge variant="outline">{t.item_count} items</Badge>
              </CardContent>
            </Card>
          ))}
        </div>

        <div className="space-y-1.5">
          <Label>Tech stack</Label>
          <Select value={selectedStack} onValueChange={setSelectedStack}>
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {stacks.map((s) => (
                <SelectItem key={s.slug} value={s.slug}>
                  {s.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="flex items-center gap-3">
          <Button onClick={() => mutation.mutate()} disabled={mutation.isPending}>
            {mutation.isPending ? 'Creating\u2026' : 'Create demo project'}
          </Button>
          <Button variant="outline" onClick={() => handleOpenChange(false)}>
            Cancel
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
