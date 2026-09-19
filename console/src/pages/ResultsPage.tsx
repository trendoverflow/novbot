import { useMemo, useState } from 'react'
import { api } from '../api/client'
import { ResultsTable } from '../components/ResultsTable'
import { Button, ErrorBanner, Loading, Toolbar } from '../components/Ui'
import { useAsync } from '../hooks/useAsync'

export function ResultsPage() {
  const [nodeFilter, setNodeFilter] = useState('')
  const [limit, setLimit] = useState(50)

  const query = useMemo(
    () => ({
      node_id: nodeFilter.trim() || undefined,
      limit,
    }),
    [nodeFilter, limit],
  )

  const state = useAsync(() => api.listResults(query), [query.node_id, query.limit])

  return (
    <section>
      <h1 className="page-title">Results</h1>
      <p className="page-lead">
        Global results from <code>GET /v1/results</code>
        {query.node_id ? (
          <>
            {' '}
            (filtered <code>node_id={query.node_id}</code>)
          </>
        ) : null}
        .
      </p>

      <Toolbar>
        <label className="field inline">
          <span className="field-label">Node ID</span>
          <input
            className="input"
            value={nodeFilter}
            placeholder="optional"
            onChange={(e) => setNodeFilter(e.target.value)}
          />
        </label>
        <label className="field inline">
          <span className="field-label">Limit</span>
          <input
            className="input input-sm"
            type="number"
            min={1}
            max={500}
            value={limit}
            onChange={(e) => setLimit(Number(e.target.value) || 50)}
          />
        </label>
        <Button variant="ghost" onClick={state.reload} disabled={state.status === 'loading'}>
          Refresh
        </Button>
      </Toolbar>

      <div className="card">
        {state.status === 'loading' ? <Loading /> : null}
        {state.status === 'error' ? <ErrorBanner error={state.error} /> : null}
        {state.status === 'ready' ? (
          <ResultsTable rows={state.data} showNode />
        ) : null}
      </div>
    </section>
  )
}
