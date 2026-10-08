import { useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { api, type SkillPublishResponse } from '../api/client'
import { SkillChangeDialog } from '../components/SkillChangeDialog'
import { UploadSkillButton } from '../components/UploadSkillButton'
import {
  Button,
  EmptyState,
  ErrorBanner,
  Loading,
  SuccessBanner,
  Toolbar,
} from '../components/Ui'
import { useAsync } from '../hooks/useAsync'
import {
  capabilityPromptLine,
  defaultInstallVersion,
  isBuiltinSource,
  sortCapabilities,
  sourceLabel,
} from '../skills/present'

type DialogAction = 'install' | 'rollback' | 'uninstall'

const READ_ONLY = 'Built-in, ships with the node'

export function SkillDetailPage() {
  const { name = '' } = useParams()
  const state = useAsync(() => api.getSkill(name), [name])
  const [pickedVersion, setPickedVersion] = useState('')
  const [dialog, setDialog] = useState<DialogAction | null>(null)
  const [uploaded, setUploaded] = useState<SkillPublishResponse | null>(null)
  const skill = state.status === 'ready' ? state.data : null
  const selectedVersion =
    skill && skill.versions.some((row) => row.version === pickedVersion)
      ? pickedVersion
      : skill
        ? defaultInstallVersion(skill.versions)
        : ''

  const versionState = useAsync(async () => {
    if (!selectedVersion) return null
    return api.getSkillVersion(name, selectedVersion)
  }, [name, selectedVersion])

  const readOnly = skill ? isBuiltinSource(skill.source) : false
  const version =
    versionState.status === 'ready' ? versionState.data : null
  const capabilities = sortCapabilities(
    version?.capabilities ?? skill?.capabilities ?? [],
  )

  return (
    <section>
      <p className="muted small">
        <Link to="/skills">Skills Hub</Link>
      </p>
      <h1 className="page-title">{skill?.display_name || name}</h1>
      <p className="page-lead">
        {skill?.description || 'Skill detail from the center catalog.'}
      </p>

      {state.status === 'loading' ? <Loading /> : null}
      {state.status === 'error' ? <ErrorBanner error={state.error} /> : null}

      {skill ? (
        <>
          <div className="card" style={{ marginBottom: '1rem' }}>
            <dl className="kv">
              <dt>Name</dt>
              <dd className="mono">{skill.name}</dd>
              <dt>Source</dt>
              <dd>{sourceLabel(skill.source)}</dd>
              <dt>Latest version</dt>
              <dd className="mono">{skill.latest_version || '—'}</dd>
              <dt>sha256</dt>
              <dd className="mono">{skill.sha256 || '—'}</dd>
              <dt>Signature</dt>
              <dd>
                {version?.signature_status === 'none' || !version
                  ? 'Not verified (OSS)'
                  : version.signature_status}
              </dd>
            </dl>
            {readOnly ? (
              <p className="muted">{READ_ONLY}. Upload, upgrade, rollback, and uninstall are disabled.</p>
            ) : null}
            <Toolbar>
              <Button variant="ghost" onClick={state.reload}>
                Refresh
              </Button>
              <UploadSkillButton
                disabled={readOnly}
                onUploaded={(body) => {
                  setUploaded(body)
                  state.reload()
                }}
              />
              <Button
                disabled={readOnly || skill.versions.length === 0}
                title={readOnly ? READ_ONLY : undefined}
                onClick={() => setDialog('install')}
              >
                Install…
              </Button>
              <Button
                variant="ghost"
                disabled={readOnly || skill.versions.length === 0}
                title={readOnly ? READ_ONLY : undefined}
                onClick={() => setDialog('install')}
              >
                Upgrade…
              </Button>
              <Button
                variant="ghost"
                disabled={readOnly}
                title={readOnly ? READ_ONLY : undefined}
                onClick={() => setDialog('rollback')}
              >
                Roll back
              </Button>
              <Button
                variant="ghost"
                disabled={readOnly}
                title={readOnly ? READ_ONLY : undefined}
                onClick={() => setDialog('uninstall')}
              >
                Uninstall…
              </Button>
            </Toolbar>
            {uploaded ? (
              <SuccessBanner>
                Uploaded <code>{uploaded.name}</code> {uploaded.version}.
              </SuccessBanner>
            ) : null}
          </div>

          <h2 className="section-title">Capabilities</h2>
          <div className="card" style={{ marginBottom: '1rem' }}>
            {versionState.status === 'loading' && selectedVersion ? (
              <Loading label="Loading version…" />
            ) : null}
            {versionState.status === 'error' ? (
              <ErrorBanner error={versionState.error} />
            ) : null}
            {capabilities.length === 0 ? (
              <EmptyState>
                {readOnly
                  ? 'Built-in skills ship with the node and do not publish a Hub capability list.'
                  : 'This version declares no capabilities.'}
              </EmptyState>
            ) : (
              <ul className="cap-list">
                {capabilities.map((cap) => (
                  <li key={cap.grant}>
                    <span className={`pill pill-risk pill-risk-${cap.risk}`}>
                      {cap.risk}
                    </span>
                    <span>{capabilityPromptLine(cap)}</span>
                    {cap.reason ? (
                      <span className="muted"> — {cap.reason}</span>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
            {version ? (
              <p className="muted small">
                Version <code>{version.version}</code> capabilities_sha256{' '}
                <code>{version.capabilities_sha256}</code>
              </p>
            ) : null}
          </div>

          <h2 className="section-title">Versions</h2>
          <div className="card">
            {skill.versions.length === 0 ? (
              <EmptyState>No Hub versions. Built-ins follow the node binary.</EmptyState>
            ) : (
              <div className="table-wrap">
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>Version</th>
                      <th>Status</th>
                      <th>Signature</th>
                      <th>sha256</th>
                    </tr>
                  </thead>
                  <tbody>
                    {skill.versions.map((row) => (
                      <tr key={row.version}>
                        <td>
                          <button
                            type="button"
                            className={
                              row.version === selectedVersion
                                ? 'link-button link-on'
                                : 'link-button'
                            }
                            onClick={() => setPickedVersion(row.version)}
                          >
                            {row.version}
                          </button>
                        </td>
                        <td>{row.status}</td>
                        <td>
                          {row.signature_status === 'none'
                            ? 'Not verified (OSS)'
                            : row.signature_status}
                        </td>
                        <td className="mono truncate" title={row.sha256}>
                          {row.sha256}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>

          {dialog ? (
            <SkillChangeDialog
              action={dialog}
              skillName={skill.name}
              versions={skill.versions}
              onClose={() => {
                setDialog(null)
                state.reload()
              }}
            />
          ) : null}
        </>
      ) : null}
    </section>
  )
}
