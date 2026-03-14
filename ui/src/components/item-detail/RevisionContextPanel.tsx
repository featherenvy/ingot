import type { RevisionContextSummary } from '../../types/domain'
import { Timestamp } from '../Timestamp'
import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'
import { DetailList } from './DetailList'

export function RevisionContextPanel({ summary }: { summary: RevisionContextSummary }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Revision Context</CardTitle>
      </CardHeader>
      <CardContent>
        <DetailList
          items={[
            { label: 'Updated', value: <Timestamp value={summary.updated_at} /> },
            {
              label: 'Changed paths',
              value:
                summary.changed_paths.length > 0 ? (
                  <code>{summary.changed_paths.join(', ')}</code>
                ) : (
                  <span className="text-muted-foreground">none</span>
                ),
            },
            { label: 'Latest validation', value: formatResultSummary(summary.latest_validation) },
            { label: 'Latest review', value: formatResultSummary(summary.latest_review) },
            {
              label: 'Accepted results',
              value:
                summary.accepted_result_refs.length > 0 ? (
                  <code>
                    {summary.accepted_result_refs.map((result) => `${result.step_id}:${result.outcome}`).join(', ')}
                  </code>
                ) : (
                  <span className="text-muted-foreground">none</span>
                ),
            },
            { label: 'Operator notes', value: summary.operator_notes_excerpt ?? 'none' },
          ]}
        />
      </CardContent>
    </Card>
  )
}

function formatResultSummary(result: { outcome: string; summary: string } | null) {
  return result ? `${result.outcome}: ${result.summary}` : 'none'
}
