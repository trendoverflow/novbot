import { NavLink, Outlet, useParams } from 'react-router-dom'

const tabClass = ({ isActive }: { isActive: boolean }) =>
  isActive ? 'tab active' : 'tab'

export function NodeDetailPage() {
  const { nodeId = '' } = useParams()

  return (
    <section>
      <h1 className="page-title">Node: {nodeId || '—'}</h1>
      <p className="page-lead">
        Per-node management. Tabs are stubs for Config, Schedules, Dispatch, and
        Results.
      </p>
      <nav className="tabs">
        <NavLink to="config" className={tabClass}>
          Config
        </NavLink>
        <NavLink to="schedules" className={tabClass}>
          Schedules
        </NavLink>
        <NavLink to="dispatch" className={tabClass}>
          Dispatch
        </NavLink>
        <NavLink to="results" className={tabClass}>
          Results
        </NavLink>
      </nav>
      <div className="card">
        <Outlet />
      </div>
    </section>
  )
}

export function NodeConfigTab() {
  const { nodeId = '' } = useParams()
  return (
    <div>
      <h2 style={{ marginTop: 0, fontSize: '1rem' }}>Config</h2>
      <p className="placeholder">
        Stub for <code>GET/PUT /v1/nodes/{nodeId}/config</code>.
      </p>
    </div>
  )
}

export function NodeSchedulesTab() {
  const { nodeId = '' } = useParams()
  return (
    <div>
      <h2 style={{ marginTop: 0, fontSize: '1rem' }}>Schedules</h2>
      <p className="placeholder">
        Stub for <code>GET/PUT /v1/nodes/{nodeId}/schedules</code>.
      </p>
    </div>
  )
}

export function NodeDispatchTab() {
  const { nodeId = '' } = useParams()
  return (
    <div>
      <h2 style={{ marginTop: 0, fontSize: '1rem' }}>Dispatch</h2>
      <p className="placeholder">
        Stub for <code>POST /v1/nodes/{nodeId}/dispatch</code>.
      </p>
    </div>
  )
}

export function NodeResultsTab() {
  const { nodeId = '' } = useParams()
  return (
    <div>
      <h2 style={{ marginTop: 0, fontSize: '1rem' }}>Results</h2>
      <p className="placeholder">
        Stub for node-scoped results (
        <code>GET /v1/results?node_id={nodeId}</code>).
      </p>
    </div>
  )
}
