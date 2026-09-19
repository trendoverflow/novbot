import { useEffect, useState } from 'react'
import { Navigate, NavLink, Outlet, useOutletContext } from 'react-router-dom'
import { api } from '../api/client'
import {
  loadSettings,
  normalizeBaseUrl,
  notifySettingsChanged,
  saveSettings,
  type ConsoleSettings,
} from '../api/settings'
import {
  Button,
  ErrorBanner,
  SuccessBanner,
  Toolbar,
} from '../components/Ui'

type SettingsCtx = {
  draft: ConsoleSettings
  setDraft: (s: ConsoleSettings) => void
  savedAt: string | null
  setSavedAt: (v: string | null) => void
}

export function SettingsLayout() {
  const [draft, setDraft] = useState<ConsoleSettings>(() => loadSettings())
  const [savedAt, setSavedAt] = useState<string | null>(null)

  return (
    <section>
      <h1 className="page-title">Settings</h1>
      <p className="page-lead">
        Connection settings for the center HTTP API. Stored in{' '}
        <code>localStorage</code> only.
      </p>
      <div className="tabs">
        <NavLink
          to="/settings/api"
          className={({ isActive }) => (isActive ? 'tab active' : 'tab')}
        >
          API
        </NavLink>
      </div>
      <Outlet context={{ draft, setDraft, savedAt, setSavedAt } satisfies SettingsCtx} />
    </section>
  )
}

export function SettingsIndexRedirect() {
  return <Navigate to="api" replace />
}

export function SettingsApiPage() {
  const { draft, setDraft, savedAt, setSavedAt } =
    useOutletContext<SettingsCtx>()
  const [error, setError] = useState<Error | null>(null)
  const [busy, setBusy] = useState(false)
  const [authNote, setAuthNote] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    api
      .tokensStatus()
      .then((s) => {
        if (!cancelled) {
          setAuthNote(
            s.auth_required
              ? 'Center requires Bearer token (NOVBOT_API_TOKEN is set).'
              : 'Center is open (NOVBOT_API_TOKEN unset); Bearer still sent when configured.',
          )
        }
      })
      .catch(() => {
        if (!cancelled) setAuthNote(null)
      })
    return () => {
      cancelled = true
    }
  }, [savedAt])

  function onSave() {
    setError(null)
    const next = {
      baseUrl: normalizeBaseUrl(draft.baseUrl),
      token: draft.token.trim(),
    }
    if (next.baseUrl) {
      try {
        const u = new URL(next.baseUrl)
        if (u.protocol !== 'http:' && u.protocol !== 'https:') {
          setError(new Error('API base URL must be http:// or https://'))
          return
        }
      } catch {
        setError(new Error('API base URL is not a valid URL'))
        return
      }
    }
    saveSettings(next)
    setDraft(next)
    setSavedAt(new Date().toLocaleString())
    notifySettingsChanged()
  }

  async function onCreateToken() {
    setBusy(true)
    setError(null)
    try {
      const res = await api.createToken()
      setDraft({ ...draft, token: res.token })
    } catch (e) {
      setError(e instanceof Error ? e : new Error(String(e)))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="card form-stack">
      <h2 className="section-title">API connection</h2>
      <p className="muted">
        Leave base URL empty to use relative <code>/v1</code> (Vite proxy in
        dev, same-origin in production). Set an absolute{' '}
        <code>http://</code> or <code>https://</code> origin to talk to a
        remote center.
      </p>

      {authNote ? <p className="muted">{authNote}</p> : null}
      {error ? <ErrorBanner error={error} /> : null}
      {savedAt ? (
        <SuccessBanner>Saved at {savedAt}.</SuccessBanner>
      ) : null}

      <label className="field">
        <span className="field-label">API base URL</span>
        <input
          className="input"
          placeholder="(empty = relative /v1)"
          value={draft.baseUrl}
          onChange={(e) => setDraft({ ...draft, baseUrl: e.target.value })}
          autoComplete="off"
          spellCheck={false}
        />
      </label>

      <label className="field">
        <span className="field-label">Bearer token</span>
        <input
          className="input"
          type="password"
          placeholder="Paste or create a token"
          value={draft.token}
          onChange={(e) => setDraft({ ...draft, token: e.target.value })}
          autoComplete="off"
          spellCheck={false}
        />
      </label>

      <Toolbar>
        <Button onClick={onSave}>Save</Button>
        <Button variant="ghost" onClick={onCreateToken} disabled={busy}>
          {busy ? 'Creating…' : 'Create token'}
        </Button>
        <Button
          variant="ghost"
          onClick={() => {
            setDraft({ baseUrl: '', token: '' })
            setSavedAt(null)
          }}
        >
          Clear form
        </Button>
      </Toolbar>
      <p className="muted">
        Create token calls <code>POST /v1/tokens</code> (stub; not stored on
        center). To enforce auth, set <code>NOVBOT_API_TOKEN</code> on the
        center to the same value, then Save here.
      </p>
    </div>
  )
}
