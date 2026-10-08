import { useState } from 'react'
import { Link } from 'react-router-dom'
import { api, type SkillPublishResponse } from '../api/client'
import { UploadSkillButton } from '../components/UploadSkillButton'
import { Button, EmptyState, ErrorBanner, Loading, SuccessBanner, Toolbar } from '../components/Ui'
import { useAsync } from '../hooks/useAsync'
import { sourceLabel } from '../skills/present'

export function SkillsListPage() {
  const state = useAsync(() => api.listSkills(), [])
  const [uploaded, setUploaded] = useState<SkillPublishResponse | null>(null)

  return (
    <section>
      <h1 className="page-title">Skills Hub</h1>
      <p className="page-lead">
        Hub skills and built-ins from <code>GET /v1/skills</code>. Open a skill
        to review capabilities and install it.
      </p>

      <Toolbar>
        <Button
          variant="ghost"
          onClick={() => {
            setUploaded(null)
            state.reload()
          }}
          disabled={state.status === 'loading'}
        >
          Refresh
        </Button>
        <UploadSkillButton
          onUploaded={(body) => {
            setUploaded(body)
            state.reload()
          }}
        />
      </Toolbar>

      {uploaded ? (
        <SuccessBanner>
          Uploaded <code>{uploaded.name}</code> {uploaded.version}. Signature
          status <code>{uploaded.signature_status}</code>.
        </SuccessBanner>
      ) : null}

      {state.status === 'loading' ? <Loading /> : null}
      {state.status === 'error' ? <ErrorBanner error={state.error} /> : null}

      {state.status === 'ready' ? (
        state.data.items.length === 0 ? (
          <div className="card">
            <EmptyState>
              No skills yet. Publish one with <code>novbot-skill pack</code> and{' '}
              <code>novbot-skill publish</code>, or upload a{' '}
              <code>.nbskill</code> package here.
            </EmptyState>
          </div>
        ) : (
          <div className="card table-wrap">
            <table className="data-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Source</th>
                  <th>Version</th>
                </tr>
              </thead>
              <tbody>
                {state.data.items.map((item) => (
                  <tr key={`${item.source}:${item.name}`}>
                    <td>
                      <Link to={`/skills/${encodeURIComponent(item.name)}`}>
                        {item.name}
                      </Link>
                    </td>
                    <td>
                      <span className={item.source === 'builtin' ? 'pill' : 'pill pill-ok'}>
                        {sourceLabel(item.source)}
                      </span>
                    </td>
                    <td className="mono">{item.latest_version || '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {state.data.note ? (
              <p className="muted small">{state.data.note}</p>
            ) : null}
          </div>
        )
      ) : null}
    </section>
  )
}
