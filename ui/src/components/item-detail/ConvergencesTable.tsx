import { shortOid } from '../../lib/git'
import type { Convergence } from '../../types/domain'
import { TooltipValue } from '../TooltipValue'
import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../ui/table'

export function ConvergencesTable({ convergences }: { convergences: Convergence[] }) {
  return (
    <Card className="gap-0">
      <CardHeader className="border-b">
        <CardTitle>Convergences ({convergences.length})</CardTitle>
      </CardHeader>
      <CardContent className="px-0">
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
                <TableCell>{convergence.status}</TableCell>
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
      </CardContent>
    </Card>
  )
}
