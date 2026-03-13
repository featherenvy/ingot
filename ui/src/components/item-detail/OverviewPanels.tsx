import type { Evaluation, Item, ItemRevision } from '../../types/domain'
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
              { label: 'Lifecycle', value: item.lifecycle_state },
              { label: 'Parking', value: item.parking_state },
              { label: 'Approval', value: item.approval_state },
              { label: 'Escalation', value: item.escalation_state },
              { label: 'Origin', value: item.origin_kind },
              { label: 'Origin finding', value: item.origin_finding_id ?? 'none' },
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
              { label: 'Board', value: <Badge variant="outline">{evaluation.board_status}</Badge> },
              { label: 'Step', value: evaluation.current_step_id ?? 'none' },
              { label: 'Phase', value: evaluation.current_phase_kind ?? 'none' },
              { label: 'Phase status', value: evaluation.phase_status ?? 'none' },
              { label: 'Next action', value: evaluation.next_recommended_action },
              { label: 'Dispatchable', value: evaluation.dispatchable_step_id ?? 'none' },
              {
                label: 'Auxiliary',
                value:
                  evaluation.auxiliary_dispatchable_step_ids.length > 0
                    ? evaluation.auxiliary_dispatchable_step_ids.join(', ')
                    : 'none',
              },
              {
                label: 'Badges',
                value: evaluation.attention_badges.length > 0 ? evaluation.attention_badges.join(', ') : 'none',
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
              { label: 'No.', value: revision.revision_no },
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
