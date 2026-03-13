import { CodeBlock } from '../CodeBlock'
import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'

export function AcceptanceCriteriaSection({ acceptanceCriteria }: { acceptanceCriteria: string }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Acceptance Criteria</CardTitle>
      </CardHeader>
      <CardContent>
        <CodeBlock
          value={acceptanceCriteria}
          wrap
          copyLabel="Copy acceptance criteria"
          maxHeightClassName="max-h-64"
          preClassName="text-sm"
        />
      </CardContent>
    </Card>
  )
}
