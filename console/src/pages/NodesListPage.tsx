import { Link } from 'react-router-dom'

/** Demo rows for scaffold navigation only (not live API data). */
const DEMO_NODES = ['demo-1', 'demo-2']

export function NodesListPage() {
  return (
    <section>
      <h1 className="page-title">Nodes</h1>
      <p className="page-lead">
        Registered nodes. List will call <code>GET /v1/nodes</code>.
      </p>
      <div className="card">
        <ul style={{ margin: 0, paddingLeft: '1.2rem' }}>
          {DEMO_NODES.map((id) => (
            <li key={id} style={{ marginBottom: '0.4rem' }}>
              <Link to={`/nodes/${encodeURIComponent(id)}`}>{id}</Link>
            </li>
          ))}
        </ul>
        <p className="placeholder" style={{ marginTop: '1rem', marginBottom: 0 }}>
          Placeholder list for layout / routing. Replace with live API data.
        </p>
      </div>
    </section>
  )
}
