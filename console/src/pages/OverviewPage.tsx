import { Link } from 'react-router-dom'
import { api, formatTime } from '../api/client'
import { ResultsTable } from '../components/ResultsTable'
import { Button, ErrorBanner, Loading, Toolbar } from '../components/Ui'
import { useAsync } from '../hooks/useAsync'

export function OverviewPage() {
  const state = useAsync(async () => {
    const [health, nodes, results] = await Promise.all([
      api.health().catch((err: unknown) => {
        throw err instanceof Error ? err : new Error(String(err))
      }),
      api.listNodes(),
      api.listResults({ limit: 10 }),
    ])
    return { health, nodes, results }
  }, [])

  return (
    <section>
      <h1 className="page-title">Overview</h1>
      <p className="page-lead">
        Aggregates from <code>/health</code>, <code>GET /v1/nodes</code>, and{' '}
        <code>GET /v1/results</code>.
      </p>

      <Toolbar>
        <Button variant="ghost" onClick={state.reload} disabled={state.status === 'loading'}>
          Refresh
        </Button>
      </Toolbar>

      {state.status === 'loading' ? <Loading /> : null}
      {state.status === 'error' ? <ErrorBanner error={state.error} /> : null}

      {state.status === 'ready' ? (
        <>
          <div className="stat-grid">
            <div className="card stat-card">
              <div className="stat-label">Center health</div>
              <div className="stat-value">
                {state.data.health.ok ? (
                  <span className="pill pill-ok">ok</span>
                ) : (
                  <span className="pill pill-err">down</span>
                )}
              </div>
              <div className="muted small">
                {state.data.health.service ?? 'novbot-center'}
              </div>
            </div>
            <div className="card stat-card">
              <div className="stat-label">Nodes</div>
              <div className="stat-value">{state.data.nodes.length}</div>
              <div className="muted small">
                <Link to="/nodes">View all</Link>
              </div>
            </div>
            <div className="card stat-card">
              <div className="stat-label">Recent results</div>
              <div className="stat-value">{state.data.results.length}</div>
              <div className="muted small">
                <Link to="/results">Open feed</Link>
              </div>
            </div>
            <div className="card stat-card">
              <div className="stat-label">Latest seen</div>
              <div className="stat-value small-val">
                {latestSeen(state.data.nodes)}
              </div>
              <div className="muted small">across registered nodes</div>
            </div>
          </div>

          <h2 className="section-title">Latest results</h2>
          <div className="card">
            <ResultsTable rows={state.data.results} showNode />
          </div>
        </>
      ) : null}
    </section>
  )
}

function latestSeen(
  nodes: { last_seen_at: string | null }[],
): string {
  let best: string | null = null
  for (const n of nodes) {
    if (!n.last_seen_at) continue
    if (!best || n.last_seen_at > best) best = n.last_seen_at
  }
  return formatTime(best)
}
