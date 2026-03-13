import { Alert, AlertAction, AlertDescription, AlertTitle } from './ui/alert'
import { Button } from './ui/button'

type PageQueryErrorProps = {
  title: string
  error: unknown
  onRetry: () => Promise<unknown>
  isRetrying?: boolean
}

export function PageQueryError({ title, error, onRetry, isRetrying = false }: PageQueryErrorProps): React.JSX.Element {
  return (
    <Alert variant="destructive">
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription className="pr-20">{String(error)}</AlertDescription>
      <AlertAction>
        <Button type="button" size="sm" variant="outline" onClick={() => void onRetry()} disabled={isRetrying}>
          {isRetrying ? 'Retrying…' : 'Retry'}
        </Button>
      </AlertAction>
    </Alert>
  )
}
