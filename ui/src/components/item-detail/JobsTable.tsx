import { shortId } from '../../lib/git'
import type { Job } from '../../types/domain'
import { DataTable } from '../DataTable'
import { StatusBadge } from '../StatusBadge'
import { TooltipValue } from '../TooltipValue'
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
    <DataTable title={`Jobs (${jobs.length})`}>
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
                <StatusBadge status={job.status} />
              </TableCell>
              <TableCell>{job.outcome_class ? <StatusBadge status={job.outcome_class} /> : '—'}</TableCell>
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
    </DataTable>
  )
}
