import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'

export function DiagnosticsSection({ diagnostics }: { diagnostics: string[] }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Diagnostics</CardTitle>
      </CardHeader>
      <CardContent>
        <ul className="list-disc space-y-2 pl-5 text-sm text-muted-foreground">
          {diagnostics.map((diagnostic) => (
            <li key={diagnostic}>{diagnostic}</li>
          ))}
        </ul>
      </CardContent>
    </Card>
  )
}
