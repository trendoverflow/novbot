import { NavLink, Outlet } from 'react-router-dom'
import { useEffect, useState } from 'react'
import { loadSettings, settingsEventName } from '../api/settings'
import './AppLayout.css'

const navClass = ({ isActive }: { isActive: boolean }) =>
  isActive ? 'nav-link active' : 'nav-link'

export function AppLayout() {
  const [conn, setConn] = useState(() => loadSettings())

  useEffect(() => {
    const sync = () => setConn(loadSettings())
    window.addEventListener(settingsEventName(), sync)
    window.addEventListener('storage', sync)
    return () => {
      window.removeEventListener(settingsEventName(), sync)
      window.removeEventListener('storage', sync)
    }
  }, [])

  const foot = conn.baseUrl
    ? conn.baseUrl
    : '/v1 same-origin'
  const tokenHint = conn.token ? ' · Bearer on' : ''

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
          <NavLink to="/fleet/skill-groups" className={navClass}>
            Fleet
          </NavLink>
          <NavLink to="/settings" className={navClass}>
            Settings
          </NavLink>
        </nav>
        <div className="sidebar-foot">
          <code title={foot}>
            {foot.length > 28 ? `${foot.slice(0, 26)}…` : foot}
          </code>
          {tokenHint}
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
