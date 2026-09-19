import { Link } from 'react-router-dom'
import { api, formatTime } from '../api/client'
import { Button, EmptyState, ErrorBanner, Loading, Toolbar } from '../components/Ui'
import { useAsync } from '../hooks/useAsync'

export function NodesListPage() {
  const state = useAsync(() => api.listNodes(), [])

  return (
    <section>
      <h1 className="page-title">Nodes</h1>
      <p className="page-lead">
        Registered nodes from <code>GET /v1/nodes</code>.
      </p>

      <Toolbar>
        <Button variant="ghost" onClick={state.reload} disabled={state.status === 'loading'}>
          Refresh
        </Button>
      </Toolbar>

      {state.status === 'loading' ? <Loading /> : null}
      {state.status === 'error' ? <ErrorBanner error={state.error} /> : null}

      {state.status === 'ready' ? (
        state.data.length === 0 ? (
          <div className="card">
            <EmptyState>
              No nodes yet. A node appears after it connects over gRPC, or after{' '}
              <code>PUT /v1/nodes/:id/config</code>.
            </EmptyState>
          </div>
        ) : (
          <div className="card table-wrap">
            <table className="data-table">
              <thead>
                <tr>
                  <th>Node ID</th>
                  <th>Hostname</th>
                  <th>Version</th>
                  <th>Last seen</th>
                  <th>Labels</th>
                </tr>
              </thead>
              <tbody>
                {state.data.map((n) => (
                  <tr key={n.node_id}>
                    <td>
                      <Link to={`/nodes/${encodeURIComponent(n.node_id)}`}>
                        {n.node_id}
                      </Link>
                    </td>
                    <td>{n.hostname || '—'}</td>
                    <td className="mono">{n.version || '—'}</td>
                    <td className="nowrap">{formatTime(n.last_seen_at)}</td>
                    <td>
                      <code className="labels">
                        {JSON.stringify(n.labels ?? {})}
                      </code>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )
      ) : null}
    </section>
  )
}
