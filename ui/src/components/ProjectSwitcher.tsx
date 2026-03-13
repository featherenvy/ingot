import { useQuery } from '@tanstack/react-query'
import { CheckIcon, ChevronsUpDownIcon, PlusIcon } from 'lucide-react'
import { useId, useState } from 'react'
import { useNavigate } from 'react-router'
import { projectsQuery } from '../api/queries'
import { cn } from '../lib/utils'
import { ProjectColorDot } from './ProjectColorDot'
import { RegisterProjectDialog } from './RegisterProjectDialog'
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from './ui/command'
import { Button } from './ui/button'
import { Popover, PopoverContent, PopoverTrigger } from './ui/popover'

type ProjectSwitcherProps = {
  activeProjectId: string | null
}

export function ProjectSwitcher({ activeProjectId }: ProjectSwitcherProps): React.JSX.Element {
  const [open, setOpen] = useState(false)
  const [registerOpen, setRegisterOpen] = useState(false)
  const popoverContentId = useId()
  const navigate = useNavigate()
  const { data: projects } = useQuery(projectsQuery())

  const activeProject = projects?.find((p) => p.id === activeProjectId)

  function selectProject(projectId: string) {
    setOpen(false)
    navigate(`/projects/${projectId}`)
  }

  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button
            type="button"
            variant="outline"
            role="combobox"
            aria-expanded={open}
            aria-controls={popoverContentId}
            aria-label="Switch project"
            className={cn(
              'h-8 gap-2 border-border/60 bg-background/50 px-2.5 text-sm hover:bg-accent/50',
              !activeProject && 'text-muted-foreground',
            )}
          >
            {activeProject ? (
              <>
                <ProjectColorDot color={activeProject.color} className="size-2.5" />
                <span className="max-w-[160px] truncate font-medium">{activeProject.name}</span>
              </>
            ) : (
              <span>Select project</span>
            )}
            <ChevronsUpDownIcon className="size-3.5 shrink-0 opacity-40" />
          </Button>
        </PopoverTrigger>
        <PopoverContent id={popoverContentId} className="w-64 p-0" align="start">
          <Command shouldFilter>
            <CommandInput placeholder="Search projects…" />
            <CommandList>
              <CommandEmpty>No projects found.</CommandEmpty>
              <CommandGroup>
                {projects?.map((project) => (
                  <CommandItem
                    key={project.id}
                    value={`${project.name} ${project.id}`}
                    onSelect={() => selectProject(project.id)}
                    className="gap-2.5"
                  >
                    <ProjectColorDot color={project.color} className="size-2.5" />
                    <span className="flex-1 truncate">{project.name}</span>
                    <CheckIcon
                      className={cn('size-3.5 shrink-0', activeProjectId === project.id ? 'opacity-100' : 'opacity-0')}
                    />
                  </CommandItem>
                ))}
              </CommandGroup>
            </CommandList>
          </Command>
          <div className="border-t p-1">
            <button
              type="button"
              onPointerDown={(e) => {
                e.preventDefault()
                setOpen(false)
                setRegisterOpen(true)
              }}
              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              <PlusIcon className="size-3.5" />
              Add new project
            </button>
          </div>
        </PopoverContent>
      </Popover>
      <RegisterProjectDialog open={registerOpen} onOpenChange={setRegisterOpen} />
    </>
  )
}
