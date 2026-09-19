import { NavLink, Outlet } from 'react-router-dom'
import './AppLayout.css'

const navClass = ({ isActive }: { isActive: boolean }) =>
  isActive ? 'nav-link active' : 'nav-link'

export function AppLayout() {
  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark">NB</span>
          <div>
            <div className="brand-title">NovBot</div>
            <div className="brand-sub">Center console</div>
          </div>
        </div>
        <nav className="nav">
          <NavLink to="/" end className={navClass}>
            Overview
          </NavLink>
          <NavLink to="/nodes" className={navClass}>
            Nodes
          </NavLink>
          <NavLink to="/results" className={navClass}>
            Results
          </NavLink>
        </nav>
        <div className="sidebar-foot">
          <code>/v1</code> same-origin
        </div>
      </aside>
      <div className="main">
        <header className="topbar">
          <span className="topbar-hint">OSS center console · live /v1</span>
        </header>
        <main className="content">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
