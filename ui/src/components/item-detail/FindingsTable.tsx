import type { Finding } from '../../types/domain'
import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../ui/table'

export function FindingsTable({ findings }: { findings: Finding[] }) {
  return (
    <Card className="gap-0">
      <CardHeader className="border-b">
        <CardTitle>Findings ({findings.length})</CardTitle>
      </CardHeader>
      <CardContent className="px-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>ID</TableHead>
              <TableHead>Severity</TableHead>
              <TableHead>Subject</TableHead>
              <TableHead>Triage</TableHead>
              <TableHead>Summary</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {findings.map((finding) => (
              <TableRow key={finding.id}>
                <TableCell>
                  <code>{finding.id}</code>
                </TableCell>
                <TableCell>{finding.severity}</TableCell>
                <TableCell>{finding.source_subject_kind}</TableCell>
                <TableCell>{finding.triage_state}</TableCell>
                <TableCell className="whitespace-normal">{finding.summary}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  )
}
