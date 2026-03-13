import { useState } from 'react'
import type { Finding, FindingTriageState } from '../../types/domain'
import { DataTable } from '../DataTable'
import { Button } from '../ui/button'
import { Input } from '../ui/input'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '../ui/table'

const selectableStates: FindingTriageState[] = [
  'fix_now',
  'wont_fix',
  'backlog',
  'duplicate',
  'dismissed_invalid',
  'needs_investigation',
]

export function FindingsTable({
  findings,
  onTriage,
  pendingFindingId,
}: {
  findings: Finding[]
  onTriage: (
    findingId: string,
    payload: { triage_state: FindingTriageState; triage_note?: string; linked_item_id?: string },
  ) => void
  pendingFindingId: string | null
}) {
  return (
    <DataTable title={`Findings (${findings.length})`}>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>ID</TableHead>
            <TableHead>Severity</TableHead>
            <TableHead>Subject</TableHead>
            <TableHead>Triage</TableHead>
            <TableHead>Summary</TableHead>
            <TableHead>Action</TableHead>
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
              <TableCell className="whitespace-normal">
                <div>{finding.triage_state}</div>
                {finding.triage_note ? (
                  <div className="text-xs text-muted-foreground">{finding.triage_note}</div>
                ) : null}
                {finding.linked_item_id ? (
                  <div className="text-xs text-muted-foreground">linked: {finding.linked_item_id}</div>
                ) : null}
              </TableCell>
              <TableCell className="whitespace-normal">{finding.summary}</TableCell>
              <TableCell className="min-w-80">
                <FindingTriageControls
                  finding={finding}
                  onTriage={onTriage}
                  pending={pendingFindingId === finding.id}
                />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </DataTable>
  )
}

function FindingTriageControls({
  finding,
  onTriage,
  pending,
}: {
  finding: Finding
  onTriage: (
    findingId: string,
    payload: { triage_state: FindingTriageState; triage_note?: string; linked_item_id?: string },
  ) => void
  pending: boolean
}) {
  const [triageState, setTriageState] = useState<FindingTriageState>(
    selectableStates.includes(finding.triage_state) ? finding.triage_state : 'fix_now',
  )
  const [triageNote, setTriageNote] = useState(finding.triage_note ?? '')
  const [linkedItemId, setLinkedItemId] = useState(finding.linked_item_id ?? '')

  return (
    <div className="flex min-w-72 flex-col gap-2">
      <select
        className="h-9 rounded-md border border-input bg-background px-3 text-sm"
        value={triageState}
        onChange={(event) => setTriageState(event.target.value as FindingTriageState)}
        disabled={pending}
      >
        {selectableStates.map((value) => (
          <option key={value} value={value}>
            {value}
          </option>
        ))}
      </select>
      <Input
        value={triageNote}
        onChange={(event) => setTriageNote(event.target.value)}
        placeholder="Triage note"
        disabled={pending}
      />
      <Input
        value={linkedItemId}
        onChange={(event) => setLinkedItemId(event.target.value)}
        placeholder="Linked item id for duplicate or existing backlog"
        disabled={pending}
      />
      <Button
        type="button"
        size="sm"
        onClick={() =>
          onTriage(finding.id, {
            triage_state: triageState,
            triage_note: triageNote || undefined,
            linked_item_id: linkedItemId || undefined,
          })
        }
        disabled={pending}
      >
        {pending ? 'Saving…' : 'Apply'}
      </Button>
    </div>
  )
}
