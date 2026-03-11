import { useQuery } from '@tanstack/react-query'
import { useParams } from 'react-router'
import { itemDetailQuery } from '../api/queries'

export default function ItemDetailPage() {
  const { projectId, itemId } = useParams<{ projectId: string; itemId: string }>()
  const { data: detail, isLoading, error } = useQuery(itemDetailQuery(projectId!, itemId!))

  if (isLoading) return <p>Loading item...</p>
  if (error) return <p>Error: {String(error)}</p>
  if (!detail) return <p>Item not found.</p>

  const { item, current_revision, evaluation } = detail

  return (
    <div>
      <h2>{current_revision.title}</h2>
      <p style={{ color: '#666' }}>{current_revision.description}</p>

      <div style={{ display: 'flex', gap: '2rem', marginTop: '1rem' }}>
        <section>
          <h3>State</h3>
          <dl style={{ fontSize: '0.85rem' }}>
            <dt>Lifecycle</dt>
            <dd>{item.lifecycle_state}</dd>
            <dt>Parking</dt>
            <dd>{item.parking_state}</dd>
            <dt>Approval</dt>
            <dd>{item.approval_state}</dd>
            <dt>Escalation</dt>
            <dd>{item.escalation_state}</dd>
            <dt>Priority</dt>
            <dd>{item.priority}</dd>
          </dl>
        </section>

        <section>
          <h3>Evaluation</h3>
          <dl style={{ fontSize: '0.85rem' }}>
            <dt>Board</dt>
            <dd>{evaluation.board_status}</dd>
            <dt>Step</dt>
            <dd>{evaluation.current_step_id ?? 'none'}</dd>
            <dt>Next action</dt>
            <dd>{evaluation.next_recommended_action}</dd>
            <dt>Dispatchable</dt>
            <dd>{evaluation.dispatchable_step_id ?? 'none'}</dd>
            {evaluation.attention_badges.length > 0 && (
              <>
                <dt>Badges</dt>
                <dd>{evaluation.attention_badges.join(', ')}</dd>
              </>
            )}
          </dl>
        </section>

        <section>
          <h3>Revision</h3>
          <dl style={{ fontSize: '0.85rem' }}>
            <dt>No.</dt>
            <dd>{current_revision.revision_no}</dd>
            <dt>Target ref</dt>
            <dd>
              <code>{current_revision.target_ref}</code>
            </dd>
            <dt>Approval policy</dt>
            <dd>{current_revision.approval_policy}</dd>
            <dt>Seed</dt>
            <dd>
              <code>{current_revision.seed_commit_oid.slice(0, 8)}</code>
            </dd>
          </dl>
        </section>
      </div>

      <section style={{ marginTop: '1.5rem' }}>
        <h3>Acceptance Criteria</h3>
        <pre style={{ background: '#f9f9f9', padding: '0.75rem', fontSize: '0.85rem', borderRadius: 4 }}>
          {current_revision.acceptance_criteria}
        </pre>
      </section>

      {detail.jobs.length > 0 && (
        <section style={{ marginTop: '1.5rem' }}>
          <h3>Jobs ({detail.jobs.length})</h3>
          <table style={{ fontSize: '0.85rem', borderCollapse: 'collapse', width: '100%' }}>
            <thead>
              <tr>
                <th style={thStyle}>ID</th>
                <th style={thStyle}>Step</th>
                <th style={thStyle}>Phase</th>
                <th style={thStyle}>Status</th>
                <th style={thStyle}>Outcome</th>
              </tr>
            </thead>
            <tbody>
              {detail.jobs.map((job) => (
                <tr key={job.id}>
                  <td style={tdStyle}>
                    <code>{job.id}</code>
                  </td>
                  <td style={tdStyle}>{job.step_id}</td>
                  <td style={tdStyle}>{job.phase_kind}</td>
                  <td style={tdStyle}>{job.status}</td>
                  <td style={tdStyle}>{job.outcome_class ?? '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}

      {detail.convergences.length > 0 && (
        <section style={{ marginTop: '1.5rem' }}>
          <h3>Convergences ({detail.convergences.length})</h3>
          <table style={{ fontSize: '0.85rem', borderCollapse: 'collapse', width: '100%' }}>
            <thead>
              <tr>
                <th style={thStyle}>ID</th>
                <th style={thStyle}>Status</th>
                <th style={thStyle}>Prepared</th>
                <th style={thStyle}>Valid</th>
              </tr>
            </thead>
            <tbody>
              {detail.convergences.map((c) => (
                <tr key={c.id}>
                  <td style={tdStyle}>
                    <code>{c.id}</code>
                  </td>
                  <td style={tdStyle}>{c.status}</td>
                  <td style={tdStyle}>
                    <code>{c.prepared_commit_oid?.slice(0, 8) ?? '—'}</code>
                  </td>
                  <td style={tdStyle}>{c.target_head_valid ? 'yes' : 'no'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}

      {evaluation.diagnostics.length > 0 && (
        <section style={{ marginTop: '1.5rem' }}>
          <h3>Diagnostics</h3>
          <ul style={{ fontSize: '0.85rem' }}>
            {evaluation.diagnostics.map((d) => (
              <li key={d}>{d}</li>
            ))}
          </ul>
        </section>
      )}

      <section style={{ marginTop: '1.5rem' }}>
        <h3>Allowed Actions</h3>
        <div style={{ display: 'flex', gap: '0.5rem' }}>
          {evaluation.allowed_actions.length > 0 ? (
            evaluation.allowed_actions.map((a) => (
              <button type="button" key={a} style={{ fontSize: '0.85rem' }}>
                {a}
              </button>
            ))
          ) : (
            <span style={{ color: '#888', fontSize: '0.85rem' }}>none</span>
          )}
        </div>
      </section>
    </div>
  )
}

const thStyle: React.CSSProperties = { textAlign: 'left', borderBottom: '1px solid #e5e5e5', padding: '0.25rem 0.5rem' }
const tdStyle: React.CSSProperties = { padding: '0.25rem 0.5rem', borderBottom: '1px solid #f0f0f0' }
