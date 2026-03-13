import { shortId } from '../../lib/git'
import { statusVariant } from '../../lib/status'
import type { Job } from '../../types/domain'
import { TooltipValue } from '../TooltipValue'
import { Badge } from '../ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../ui/table'
import { JobActions } from './JobActions'

export function JobsTable({
  projectId,
  itemId,
  jobs,
  activeJobId,
  retryableJobIds,
  onSuccess,
}: {
  projectId: string
  itemId: string
  jobs: Job[]
  activeJobId: string | null
  retryableJobIds: Set<string>
  onSuccess: () => void
}) {
  return (
    <Card className="gap-0">
      <CardHeader className="border-b">
        <CardTitle>Jobs ({jobs.length})</CardTitle>
      </CardHeader>
      <CardContent className="px-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>ID</TableHead>
              <TableHead>Step</TableHead>
              <TableHead>Phase</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Outcome</TableHead>
              <TableHead>Actions</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {jobs.map((job) => (
              <TableRow key={job.id}>
                <TableCell>
                  <TooltipValue content={job.id}>
                    <code>{shortId(job.id)}</code>
                  </TooltipValue>
                </TableCell>
                <TableCell>{job.step_id}</TableCell>
                <TableCell>{job.phase_kind}</TableCell>
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
                  <JobActions
                    projectId={projectId}
                    itemId={itemId}
                    jobId={job.id}
                    canCancel={activeJobId === job.id}
                    canRetry={retryableJobIds.has(job.id)}
                    onSuccess={onSuccess}
                  />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  )
}
