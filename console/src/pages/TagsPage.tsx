import { Link } from 'react-router-dom'
import { api } from '../api/client'
import { Button, EmptyState, ErrorBanner, Loading, Toolbar } from '../components/Ui'
import { useAsync } from '../hooks/useAsync'
import { canonicalSelector } from '../tags/selector'

export function TagsPage() {
  const state = useAsync(() => api.tagOverview(), [])
  if (state.status === 'loading') return <Loading />
  if (state.status === 'error') return <ErrorBanner error={state.error} />

  const overview = state.data
  const missingEnv = overview.missing.env ?? 0
  const missingRole = overview.missing.role ?? 0

  return (
    <section>
      <h1 className="page-title">Tags</h1>
      <p className="page-lead">
        Keys and values across the fleet from <code>GET /v1/tags</code>. Counts use effective tags.
      </p>
      <Toolbar>
        <Button variant="ghost" onClick={state.reload}>
          Refresh
        </Button>
      </Toolbar>

      <div className="card">
        <h2 className="section-title">Missing recommended tags</h2>
        <p>
          <Link to={nodesHref('!env')}>{missingEnv} without env</Link>
          {' · '}
          <Link to={nodesHref('!role')}>{missingRole} without role</Link>
        </p>
      </div>

      <div className="card table-wrap">
        <h2 className="section-title">Keys</h2>
        {overview.keys.length === 0 ? (
          <EmptyState>No tags yet.</EmptyState>
        ) : (
          <table className="data-table">
            <thead>
              <tr>
                <th>Key</th>
                <th>Value</th>
                <th>Nodes</th>
                <th>Sources</th>
              </tr>
            </thead>
            <tbody>
              {overview.keys.flatMap((key) =>
                key.values.map((item) => (
                  <tr key={`${key.key}=${item.value}`}>
                    <td className="mono">{key.key}</td>
                    <td>
                      <Link to={nodesHref(canonicalSelector([{ key: key.key, op: 'eq', values: [item.value] }]))}>
                        {item.value}
                      </Link>
                    </td>
                    <td className="mono">{item.nodes}</td>
                    <td className="mono">{sourceMix(item.sources)}</td>
                  </tr>
                )),
              )}
            </tbody>
          </table>
        )}
      </div>

      <div className="card table-wrap">
        <h2 className="section-title">Rare values</h2>
        {overview.rare_values.length === 0 ? (
          <EmptyState>No value is used by only one node.</EmptyState>
        ) : (
          <table className="data-table">
            <thead>
              <tr>
                <th>Tag</th>
                <th>Nodes</th>
              </tr>
            </thead>
            <tbody>
              {overview.rare_values.map((item) => (
                <tr key={`${item.key}=${item.value}`}>
                  <td>
                    <Link to={nodesHref(canonicalSelector([{ key: item.key, op: 'eq', values: [item.value] }]))}>
                      <span className="mono">
                        {item.key}={item.value}
                      </span>
                    </Link>
                  </td>
                  <td className="mono">{item.nodes}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      <div className="card table-wrap">
        <h2 className="section-title">Invalid boot tags</h2>
        {overview.invalid.length === 0 ? (
          <EmptyState>No invalid boot-reported tags.</EmptyState>
        ) : (
          <table className="data-table">
            <thead>
              <tr>
                <th>Node</th>
                <th>Key</th>
                <th>Reported value</th>
                <th>Reason</th>
              </tr>
            </thead>
            <tbody>
              {overview.invalid.map((item) => (
                <tr key={`${item.node_id}:${item.key}:${item.value}`}>
                  <td>
                    <Link to={`/nodes/${encodeURIComponent(item.node_id)}/tags`}>{item.node_id}</Link>
                  </td>
                  <td className="mono">{item.key}</td>
                  <td className="mono">{item.value}</td>
                  <td className="mono">{item.invalid_reason}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </section>
  )
}

function nodesHref(selector: string): string {
  return `/nodes?selector=${encodeURIComponent(selector)}`
}

function sourceMix(sources: Record<string, number>): string {
  const parts = Object.entries(sources)
    .filter(([, count]) => count > 0)
    .map(([source, count]) => `${source} ${count}`)
  return parts.length > 0 ? parts.join(', ') : '—'
}
