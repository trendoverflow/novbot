import { Link } from 'react-router-dom'
import { formatTime, type ResultRow } from '../api/client'
import { EmptyState } from './Ui'

type Props = {
  rows: ResultRow[]
  showNode?: boolean
}

export function ResultsTable({ rows, showNode = true }: Props) {
  if (rows.length === 0) {
    return <EmptyState>No results yet.</EmptyState>
  }

  return (
    <div className="table-wrap">
      <table className="data-table">
        <thead>
          <tr>
            <th>ID</th>
            {showNode ? <th>Node</th> : null}
            <th>Spec</th>
            <th>Status</th>
            <th>Run</th>
            <th>Observed</th>
            <th>Received</th>
            <th>Payload</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.id}>
              <td className="mono">{r.id}</td>
              {showNode ? (
                <td>
                  <Link to={`/nodes/${encodeURIComponent(r.node_id)}`}>
                    {r.node_id}
                  </Link>
                </td>
              ) : null}
              <td className="mono">{r.spec_id}</td>
              <td>
                <span className={`pill pill-${statusTone(r.status)}`}>
                  {r.status}
                </span>
              </td>
              <td className="mono truncate" title={r.run_id}>
                {shortId(r.run_id)}
              </td>
              <td className="nowrap">{formatTime(r.observed_at)}</td>
              <td className="nowrap">{formatTime(r.received_at)}</td>
              <td>
                <details>
                  <summary className="muted">JSON</summary>
                  <pre className="json-pre">{pretty(r.payload)}</pre>
                </details>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function shortId(id: string): string {
  if (id.length <= 12) return id
  return `${id.slice(0, 8)}…`
}

function pretty(v: unknown): string {
  try {
    return JSON.stringify(v, null, 2)
  } catch {
    return String(v)
  }
}

function statusTone(status: string): string {
  const s = status.toLowerCase()
  if (s === 'ok' || s === 'success' || s === 'passed') return 'ok'
  if (s === 'error' || s === 'fail' || s === 'failed') return 'err'
  return 'neutral'
}
