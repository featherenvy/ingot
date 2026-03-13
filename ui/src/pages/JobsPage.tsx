import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { Link } from 'react-router'
import { agentsQuery, jobLogsQuery, projectJobsQuery } from '../api/queries'
import { LogBlock } from '../components/LogBlock'
import { PageHeaderSkeleton, TableCardSkeleton } from '../components/PageSkeletons'
import { Timestamp } from '../components/Timestamp'
import { TooltipValue } from '../components/TooltipValue'
import { Alert, AlertDescription, AlertTitle } from '../components/ui/alert'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../components/ui/table'
import { useRequiredProjectId } from '../hooks/useRequiredRouteParam'
import { getQueuedJobBlocker } from '../jobBlockers'
import { shortId } from '../lib/git'
import { statusVariant } from '../lib/status'

export default function JobsPage() {
  const projectId = useRequiredProjectId()
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null)
  const { data: jobs, isLoading } = useQuery(projectJobsQuery(projectId))
  const { data: agents } = useQuery(agentsQuery())
  const { data: logs, isLoading: isLogsLoading } = useQuery(jobLogsQuery(selectedJobId ?? ''))
  const queueBlocker = getQueuedJobBlocker(jobs ?? [], agents)

  if (isLoading) {
    return (
      <div className="space-y-6">
        <PageHeaderSkeleton width="w-28" />
        <div className="grid gap-6 xl:grid-cols-[minmax(0,1.2fr)_minmax(22rem,1fr)]">
          <TableCardSkeleton columns={5} rows={5} />
          <Card className="min-h-[24rem]">
            <CardHeader className="space-y-2">
              <Skeleton className="h-6 w-24" />
              <Skeleton className="h-4 w-full max-w-xs" />
            </CardHeader>
            <CardContent className="space-y-4">
              <Skeleton className="h-24 w-full" />
              <Skeleton className="h-24 w-full" />
            </CardContent>
          </Card>
        </div>
      </div>
    )
  }

  const selectJob = (jobId: string) => {
    setSelectedJobId(jobId)
  }

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Jobs</h2>
        <p className="text-sm text-muted-foreground">
          Inspect queued and completed runs, then drill into their logs and result payloads.
        </p>
      </div>

      {queueBlocker && (
        <Alert>
          <AlertTitle>Agents required</AlertTitle>
          <AlertDescription className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <span>{queueBlocker}</span>
            <Button asChild size="sm" variant="outline">
              <Link to={`/projects/${projectId}/config`}>Open Config</Link>
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {jobs && jobs.length > 0 ? (
        <div className="grid gap-6 xl:grid-cols-[minmax(0,1.2fr)_minmax(22rem,1fr)]">
          <Card className="gap-0">
            <CardHeader className="border-b">
              <CardTitle>Project jobs</CardTitle>
              <CardDescription>Select a row to inspect prompt output and structured results.</CardDescription>
            </CardHeader>
            <CardContent className="px-0">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>ID</TableHead>
                    <TableHead>Step</TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead>Outcome</TableHead>
                    <TableHead>Started</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {jobs.map((job) => (
                    <TableRow
                      key={job.id}
                      onClick={() => selectJob(job.id)}
                      onKeyDown={(event) => {
                        if (event.key === 'Enter' || event.key === ' ') {
                          event.preventDefault()
                          selectJob(job.id)
                        }
                      }}
                      className="cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
                      data-state={selectedJobId === job.id ? 'selected' : undefined}
                      role="button"
                      tabIndex={0}
                      aria-pressed={selectedJobId === job.id}
                      aria-label={`Select job ${job.id}`}
                    >
                      <TableCell>
                        <TooltipValue content={job.id}>
                          <code>{shortId(job.id)}</code>
                        </TooltipValue>
                      </TableCell>
                      <TableCell>{job.step_id}</TableCell>
                      <TableCell>
                        <Badge variant={statusVariant(job.status)}>{job.status}</Badge>
                      </TableCell>
                      <TableCell>
                        {job.outcome_class ? (
                          <Badge variant={statusVariant(job.outcome_class)}>{job.outcome_class}</Badge>
                        ) : (
                          '—'
                        )}
                      </TableCell>
                      <TableCell>
                        <Timestamp value={job.started_at} />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </CardContent>
          </Card>

          <Card className="min-h-[24rem]">
            <CardHeader>
              <CardTitle>Logs</CardTitle>
              <CardDescription>Prompt, stdout, stderr, and result data for the selected job.</CardDescription>
            </CardHeader>
            <CardContent>
              {!selectedJobId ? (
                <p className="text-sm text-muted-foreground">Select a job to inspect prompt and logs.</p>
              ) : isLogsLoading ? (
                <div className="grid gap-4">
                  <Skeleton className="h-24 w-full" />
                  <Skeleton className="h-24 w-full" />
                  <Skeleton className="h-24 w-full" />
                </div>
              ) : (
                <div className="grid gap-4">
                  <LogBlock label="Prompt" value={logs?.prompt} />
                  <LogBlock label="Stdout" value={logs?.stdout} />
                  <LogBlock label="Stderr" value={logs?.stderr} />
                  <LogBlock label="Result" value={logs?.result ? JSON.stringify(logs.result, null, 2) : null} />
                </div>
              )}
            </CardContent>
          </Card>
        </div>
      ) : (
        <Card>
          <CardContent className="py-6 text-sm text-muted-foreground">No jobs yet.</CardContent>
        </Card>
      )}
    </div>
  )
}
