import type { Evaluation, Item, ItemRevision } from '../../types/domain'
import { StatusBadge } from '../StatusBadge'
import { Badge } from '../ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '../ui/card'
import { DetailList } from './DetailList'

export function OverviewPanels({
  item,
  evaluation,
  revision,
}: {
  item: Item
  evaluation: Evaluation
  revision: ItemRevision
}) {
  return (
    <div className="grid gap-4 lg:grid-cols-3">
      <Card size="sm">
        <CardHeader>
          <CardTitle>State</CardTitle>
        </CardHeader>
        <CardContent>
          <DetailList
            items={[
              { label: 'Lifecycle', value: <StatusBadge status={item.lifecycle_state} /> },
              { label: 'Parking', value: <StatusBadge status={item.parking_state} /> },
              { label: 'Approval', value: <StatusBadge status={item.approval_state} /> },
              { label: 'Escalation', value: <StatusBadge status={item.escalation_state} /> },
              { label: 'Origin', value: <span className="font-mono">{item.origin_kind}</span> },
              {
                label: 'Origin finding',
                value: item.origin_finding_id ? (
                  <code>{item.origin_finding_id}</code>
                ) : (
                  <span className="text-muted-foreground">none</span>
                ),
              },
              { label: 'Priority', value: <Badge variant="secondary">{item.priority}</Badge> },
            ]}
          />
        </CardContent>
      </Card>

      <Card size="sm">
        <CardHeader>
          <CardTitle>Evaluation</CardTitle>
        </CardHeader>
        <CardContent>
          <DetailList
            items={[
              { label: 'Board', value: <StatusBadge status={evaluation.board_status} /> },
              {
                label: 'Step',
                value: evaluation.current_step_id ? (
                  <code>{evaluation.current_step_id}</code>
                ) : (
                  <span className="text-muted-foreground">none</span>
                ),
              },
              {
                label: 'Phase',
                value: evaluation.current_phase_kind ? (
                  <span className="font-mono">{evaluation.current_phase_kind}</span>
                ) : (
                  <span className="text-muted-foreground">none</span>
                ),
              },
              {
                label: 'Phase status',
                value: evaluation.phase_status ? (
                  <StatusBadge status={evaluation.phase_status} />
                ) : (
                  <span className="text-muted-foreground">none</span>
                ),
              },
              {
                label: 'Next action',
                value: <span className="font-mono">{evaluation.next_recommended_action}</span>,
              },
              {
                label: 'Dispatchable',
                value: evaluation.dispatchable_step_id ? (
                  <code>{evaluation.dispatchable_step_id}</code>
                ) : (
                  <span className="text-muted-foreground">none</span>
                ),
              },
              {
                label: 'Auxiliary',
                value:
                  evaluation.auxiliary_dispatchable_step_ids.length > 0 ? (
                    <code>{evaluation.auxiliary_dispatchable_step_ids.join(', ')}</code>
                  ) : (
                    <span className="text-muted-foreground">none</span>
                  ),
              },
              {
                label: 'Badges',
                value:
                  evaluation.attention_badges.length > 0 ? (
                    evaluation.attention_badges.join(', ')
                  ) : (
                    <span className="text-muted-foreground">none</span>
                  ),
              },
            ]}
          />
        </CardContent>
      </Card>

      <Card size="sm">
        <CardHeader>
          <CardTitle>Revision</CardTitle>
        </CardHeader>
        <CardContent>
          <DetailList
            items={[
              { label: 'No.', value: <span className="font-mono tabular-nums">{revision.revision_no}</span> },
              { label: 'Target ref', value: <code>{revision.target_ref}</code> },
              { label: 'Approval policy', value: <Badge variant="outline">{revision.approval_policy}</Badge> },
              { label: 'Seed', value: <code>{revision.seed_commit_oid.slice(0, 8)}</code> },
            ]}
          />
        </CardContent>
      </Card>
    </div>
  )
}
