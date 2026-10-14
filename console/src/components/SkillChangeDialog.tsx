import { useEffect, useState } from 'react'
import {
  ApiError,
  api,
  apiErrorCode,
  isCapabilitiesChanged,
  skillInUseRefs,
  targetChangedDetail,
  type SkillChangeResponse,
  type SkillVersionDetail,
  type SkillVersionSummary,
} from '../api/client'
import {
  capabilityPromptLine,
  defaultInstallVersion,
  generationChanged,
  outcomeDetail,
  outcomeHeadline,
  outcomePillClass,
  resultSummary,
  sortCapabilities,
} from '../skills/present'
import { Button, ErrorBanner, Loading } from './Ui'
import { TargetPicker, type PickerState } from './TargetPicker'

type Action = 'install' | 'rollback' | 'uninstall'
type Step = 'version' | 'nodes' | 'capabilities' | 'summary'

const emptyPick: PickerState = {
  ready: false,
  target: null,
  preview: null,
}

export function SkillChangeDialog({
  action,
  skillName,
  versions,
  onClose,
}: {
  action: Action
  skillName: string
  versions: SkillVersionSummary[]
  onClose: () => void
}) {
  const steps: Step[] =
    action === 'install'
      ? ['version', 'nodes', 'capabilities', 'summary']
      : action === 'rollback'
        ? ['version', 'nodes', 'summary']
        : ['nodes', 'summary']

  const [step, setStep] = useState<Step>(steps[0])
  const [version, setVersion] = useState(() =>
    action === 'rollback' ? '' : defaultInstallVersion(versions),
  )
  const [pick, setPick] = useState<PickerState>(emptyPick)
  const [refreshKey, setRefreshKey] = useState(0)
  const [drift, setDrift] = useState<{ added: string[]; removed: string[] } | null>(
    null,
  )
  const [loadedVersion, setLoadedVersion] = useState<{
    key: string
    detail: SkillVersionDetail | null
    error: Error | null
  } | null>(null)
  const [reloadTick, setReloadTick] = useState(0)
  const [reviewedKey, setReviewedKey] = useState<string | null>(null)
  const [capChanged, setCapChanged] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const [submitError, setSubmitError] = useState<Error | null>(null)
  const [inUse, setInUse] = useState<{
    specIds: string[]
    scheduleIds: string[]
  } | null>(null)
  const [force, setForce] = useState(false)
  const [result, setResult] = useState<SkillChangeResponse | null>(null)

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !submitting) onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose, submitting])

  const versionKey = `${action}\n${skillName}\n${version}\n${reloadTick}`

  useEffect(() => {
    if (action !== 'install' || !version) return
    let cancelled = false
    const key = versionKey
    api
      .getSkillVersion(skillName, version)
      .then((detail) => {
        if (!cancelled) setLoadedVersion({ key, detail, error: null })
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setLoadedVersion({
            key,
            detail: null,
            error: err instanceof Error ? err : new Error(String(err)),
          })
        }
      })
    return () => {
      cancelled = true
    }
  }, [action, skillName, version, reloadTick, versionKey])

  const versionMatches = loadedVersion?.key === versionKey
  const versionDetail = versionMatches ? loadedVersion.detail : null
  const versionError = versionMatches ? loadedVersion.error : null
  const versionLoading =
    action === 'install' && version !== '' && !versionMatches
  const displayedHash = versionDetail?.capabilities_sha256 ?? ''
  const reviewKey = `${displayedHash}\n${reloadTick}`
  const reviewed = reviewedKey === reviewKey && displayedHash !== ''
  const hashReady =
    action !== 'install' ||
    (!versionLoading &&
      versionDetail != null &&
      versionDetail.version === version &&
      displayedHash !== '')
  const grants = versionDetail ? sortCapabilities(versionDetail.capabilities) : []

  function go(delta: number) {
    const index = steps.indexOf(step)
    const next = steps[index + delta]
    if (next) setStep(next)
  }

  async function submit(forceOverride?: boolean) {
    if (!pick.ready || !pick.target || submitting || (action === 'install' && !hashReady)) {
      return
    }
    const useForce = forceOverride ?? force
    setSubmitting(true)
    setSubmitError(null)
    setInUse(null)
    try {
      const target = pick.target
      const confirm = pick.confirmProd
        ? { confirm_prod: pick.confirmProd }
        : {}
      const response =
        action === 'install'
          ? await api.installSkill(skillName, {
              version,
              target,
              ...confirm,
              accepted_capabilities_sha256: displayedHash,
            })
          : action === 'rollback'
            ? await api.rollbackSkill(skillName, {
                target,
                ...confirm,
                ...(version ? { to_version: version } : {}),
              })
            : await api.uninstallSkill(skillName, {
                target,
                ...confirm,
                force: useForce,
              })
      setCapChanged(null)
      setDrift(null)
      setResult(response)
    } catch (err) {
      if (action === 'install' && isCapabilitiesChanged(err)) {
        const message = err instanceof Error ? err.message : 'capabilities changed'
        setCapChanged(
          `The server refused this install: 409 capabilities_changed. ${message} The version was reloaded. Review the grants below and confirm again. The previous hash was not accepted.`,
        )
        setReviewedKey(null)
        setReloadTick((n) => n + 1)
        setStep('capabilities')
        return
      }
      const changed = targetChangedDetail(err)
      if (changed) {
        setDrift(changed)
        setRefreshKey((n) => n + 1)
        setStep('nodes')
        setSubmitError(
          new Error('409 target_changed. Preview again, then confirm.'),
        )
        return
      }
      const refs = skillInUseRefs(err)
      if (refs) setInUse(refs)
      const code = apiErrorCode(err)
      const message = err instanceof Error ? err.message : String(err)
      const status = err instanceof ApiError ? err.status : 0
      setSubmitError(
        new Error(code ? `${message} (${status} ${code})` : message),
      )
    } finally {
      setSubmitting(false)
    }
  }

  const title =
    action === 'install'
      ? `Install or upgrade ${skillName}`
      : action === 'rollback'
        ? `Roll back ${skillName}`
        : `Uninstall ${skillName}`

  const nextDisabled =
    (step === 'version' && action === 'install' && !version) ||
    (step === 'nodes' && !pick.ready) ||
    (step === 'capabilities' && (!reviewed || !hashReady))
  const matched = pick.preview?.node_ids ?? []

  return (
    <div className="dialog-backdrop">
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="skill-change-title"
      >
        <div className="dialog-head">
          <h2 id="skill-change-title">{title}</h2>
          <Button variant="ghost" onClick={onClose} disabled={submitting}>
            Close
          </Button>
        </div>

        {result ? (
          <ChangeResult result={result} />
        ) : (
          <>
            <ol className="step-list">
              {steps.map((name) => (
                <li key={name} className={name === step ? 'step-on' : undefined}>
                  {stepLabel(name)}
                </li>
              ))}
            </ol>

            {step === 'version' ? (
              <div>
                {action === 'install' ? (
                  <>
                    <label className="field">
                      <span className="field-label">Version</span>
                      <select
                        className="input"
                        value={version}
                        onChange={(event) => setVersion(event.target.value)}
                      >
                        {versions.map((row) => (
                          <option key={row.version} value={row.version}>
                            {row.version} ({row.status})
                          </option>
                        ))}
                      </select>
                    </label>
                    <p className="muted small">
                      Defaults to the latest published version. Install and
                      upgrade both call{' '}
                      <code>POST /v1/skills/{skillName}/install</code>.
                    </p>
                    {versionDetail ? (
                      <p className="muted small">
                        sha256 <code>{versionDetail.sha256}</code>
                        {versionDetail.platforms.length > 0
                          ? ` · platforms ${versionDetail.platforms.join(', ')}`
                          : ''}
                        {versionDetail.min_node_version
                          ? ` · min node ${versionDetail.min_node_version}`
                          : ''}
                      </p>
                    ) : null}
                    {versionLoading ? <Loading label="Loading version…" /> : null}
                    {versionError ? <ErrorBanner error={versionError} /> : null}
                  </>
                ) : (
                  <label className="field">
                    <span className="field-label">Roll back to</span>
                    <select
                      className="input"
                      value={version}
                      onChange={(event) => setVersion(event.target.value)}
                    >
                      <option value="">Previous version on each node</option>
                      {versions.map((row) => (
                        <option key={row.version} value={row.version}>
                          {row.version} ({row.status})
                        </option>
                      ))}
                    </select>
                  </label>
                )}
              </div>
            ) : null}

            {step === 'nodes' ? (
              <div>
                <TargetPicker
                  onChange={setPick}
                  refreshKey={refreshKey}
                  disabled={submitting}
                />
                {drift ? (
                  <div className="alert alert-error" role="alert">
                    <strong>409 target_changed</strong>
                    <div>
                      Added {drift.added.join(', ') || 'none'}. Removed{' '}
                      {drift.removed.join(', ') || 'none'}. The preview was
                      refreshed. Confirm again after it settles.
                    </div>
                  </div>
                ) : null}
              </div>
            ) : null}

            {step === 'capabilities' ? (
              <div>
                {capChanged ? (
                  <div className="alert alert-error" role="alert">
                    <strong>409 capabilities_changed</strong>
                    <div>{capChanged}</div>
                  </div>
                ) : null}
                {versionLoading ? <Loading label="Reloading version…" /> : null}
                {versionError ? <ErrorBanner error={versionError} /> : null}
                {versionDetail && !versionLoading ? (
                  <>
                    <p>
                      This skill will be able to do the following. Confirm this
                      list. The install sends{' '}
                      <code>accepted_capabilities_sha256</code> from this
                      version.
                    </p>
                    {grants.length === 0 ? (
                      <p className="muted">This version declares no capabilities.</p>
                    ) : (
                      <ul className="cap-list">
                        {grants.map((cap) => (
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
                    <p className="muted small">
                      capabilities_sha256 <code>{displayedHash || '—'}</code>
                    </p>
                    {versionDetail.lint_warnings.length > 0 ? (
                      <ul className="warn-list">
                        {versionDetail.lint_warnings.map((warning) => (
                          <li key={warning}>{warning}</li>
                        ))}
                      </ul>
                    ) : null}
                    <label className="check">
                      <input
                        type="checkbox"
                        checked={reviewed}
                        onChange={(event) =>
                          setReviewedKey(event.target.checked ? reviewKey : null)
                        }
                      />
                      I reviewed these capabilities
                    </label>
                  </>
                ) : null}
              </div>
            ) : null}

            {step === 'summary' ? (
              <div>
                {capChanged ? (
                  <div className="alert alert-error" role="alert">
                    <strong>409 capabilities_changed</strong>
                    <div>{capChanged}</div>
                  </div>
                ) : null}
                {drift ? (
                  <div className="alert alert-error" role="alert">
                    <strong>409 target_changed</strong>
                    <div>
                      Added {drift.added.join(', ') || 'none'}. Removed{' '}
                      {drift.removed.join(', ') || 'none'}. Go back and preview
                      again.
                    </div>
                  </div>
                ) : null}
                <p>
                  {action === 'install'
                    ? `Install ${skillName} ${version}.`
                    : action === 'rollback'
                      ? `Roll back ${skillName}${version ? ` to ${version}` : ' to the previous version on each node'}.`
                      : `Uninstall ${skillName}.`}
                </p>
                <dl className="kv">
                  <dt>target</dt>
                  <dd className="mono">
                    {pick.target?.selector
                      ? pick.target.selector
                      : pick.target?.node_ids?.join(', ') || '—'}
                  </dd>
                  <dt>matched</dt>
                  <dd className="mono">
                    {matched.length > 0 ? matched.join(', ') : '—'}
                  </dd>
                  {action === 'install' ? (
                    <>
                      <dt>accepted hash</dt>
                      <dd className="mono">{displayedHash || '—'}</dd>
                    </>
                  ) : null}
                </dl>
                <p className="muted small">
                  The matched list is frozen when you confirm. A node the center
                  sees as offline returns <strong>queued</strong>.
                </p>
                {action === 'install' && grants.length > 0 ? (
                  <ul className="cap-list">
                    {grants.map((cap) => (
                      <li key={cap.grant}>{capabilityPromptLine(cap)}</li>
                    ))}
                  </ul>
                ) : null}
                {action === 'install' && !reviewed ? (
                  <p className="muted small">
                    Review the capabilities again before confirming.
                  </p>
                ) : null}
                {action === 'uninstall' ? (
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={force}
                      onChange={(event) => setForce(event.target.checked)}
                    />
                    Force uninstall even if a spec references this skill
                  </label>
                ) : null}
                {submitError ? <ErrorBanner error={submitError} /> : null}
                {inUse ? (
                  <div className="alert alert-error" role="alert">
                    <strong>409 skill_in_use</strong>
                    <div>
                      Specs: {inUse.specIds.join(', ') || '—'}
                      {inUse.scheduleIds.length > 0
                        ? `. Schedules: ${inUse.scheduleIds.join(', ')}`
                        : ''}
                    </div>
                    <div className="row-gap" style={{ marginTop: '0.6rem' }}>
                      <Button
                        onClick={() => void submit(true)}
                        disabled={submitting}
                      >
                        Uninstall with force
                      </Button>
                    </div>
                  </div>
                ) : null}
              </div>
            ) : null}

            <div className="dialog-actions">
              {steps.indexOf(step) > 0 ? (
                <Button variant="ghost" onClick={() => go(-1)} disabled={submitting}>
                  Back
                </Button>
              ) : null}
              {step !== 'summary' ? (
                <Button onClick={() => go(1)} disabled={nextDisabled}>
                  Next
                </Button>
              ) : (
                <Button
                  onClick={() => void submit()}
                  disabled={
                    submitting ||
                    !pick.ready ||
                    !hashReady ||
                    (action === 'install' && !reviewed)
                  }
                >
                  {submitting ? 'Sending…' : 'Confirm'}
                </Button>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  )
}

function stepLabel(step: Step): string {
  switch (step) {
    case 'version':
      return 'Version'
    case 'nodes':
      return 'Nodes'
    case 'capabilities':
      return 'Capabilities'
    case 'summary':
      return 'Summary'
  }
}

function ChangeResult({ result }: { result: SkillChangeResponse }) {
  const already = result.per_node.filter((row) => row.outcome === 'already_installed')
  const changed = result.per_node.some((row) => generationChanged(row.outcome))
  return (
    <div>
      <div className={changed ? 'alert alert-ok' : 'alert alert-note'} role="status">
        {resultSummary(result.per_node)}
      </div>
      {result.operation_id ? (
        <p className="muted small">
          Operation <code>{result.operation_id}</code> was recorded.
          {result.resolution_id ? (
            <>
              {' '}
              Resolution <code>{result.resolution_id}</code>.
            </>
          ) : null}
          {already.length > 0
            ? ' An operation record does not mean the desired generation changed.'
            : ''}
        </p>
      ) : (
        <p className="muted small">No operation id was returned.</p>
      )}
      <div className="table-wrap">
        <table className="data-table">
          <thead>
            <tr>
              <th>Node</th>
              <th>Outcome</th>
              <th>Generation</th>
            </tr>
          </thead>
          <tbody>
            {result.per_node.map((row) => (
              <tr key={row.node_id}>
                <td className="mono">{row.node_id}</td>
                <td>
                  <span className={outcomePillClass(row.outcome)}>
                    {outcomeHeadline(row.outcome)}
                  </span>
                  <div className="small">{outcomeDetail(row.outcome, row.generation)}</div>
                </td>
                <td className="mono">
                  {generationChanged(row.outcome)
                    ? `${row.generation} updated`
                    : `${row.generation} unchanged`}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}
