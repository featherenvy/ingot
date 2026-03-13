import { shortOid } from '../../lib/git'
import type { Convergence } from '../../types/domain'
import { DataTable } from '../DataTable'
import { StatusBadge } from '../StatusBadge'
import { TooltipValue } from '../TooltipValue'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../ui/table'

export function ConvergencesTable({ convergences }: { convergences: Convergence[] }) {
  return (
    <DataTable title={`Convergences (${convergences.length})`}>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>ID</TableHead>
            <TableHead>Status</TableHead>
            <TableHead>Input target</TableHead>
            <TableHead>Prepared</TableHead>
            <TableHead>Final target</TableHead>
            <TableHead>Valid</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {convergences.map((convergence) => (
            <TableRow key={convergence.id}>
              <TableCell>
                <code>{convergence.id}</code>
              </TableCell>
              <TableCell>
                <StatusBadge status={convergence.status} />
              </TableCell>
              <TableCell>
                <TooltipValue content={convergence.input_target_commit_oid}>
                  <code>{shortOid(convergence.input_target_commit_oid)}</code>
                </TooltipValue>
              </TableCell>
              <TableCell>
                <TooltipValue content={convergence.prepared_commit_oid}>
                  <code>{shortOid(convergence.prepared_commit_oid)}</code>
                </TooltipValue>
              </TableCell>
              <TableCell>
                <TooltipValue content={convergence.final_target_commit_oid}>
                  <code>{shortOid(convergence.final_target_commit_oid)}</code>
                </TooltipValue>
              </TableCell>
              <TableCell>{convergence.target_head_valid ? 'yes' : 'no'}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </DataTable>
  )
}
