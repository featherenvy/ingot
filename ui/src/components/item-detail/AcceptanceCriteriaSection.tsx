import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'

export function AcceptanceCriteriaSection({ acceptanceCriteria }: { acceptanceCriteria: string }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Acceptance Criteria</CardTitle>
      </CardHeader>
      <CardContent>
        <pre className="rounded-lg border border-border bg-muted/30 p-4 text-sm leading-6 whitespace-pre-wrap">
          {acceptanceCriteria}
        </pre>
      </CardContent>
    </Card>
  )
}
