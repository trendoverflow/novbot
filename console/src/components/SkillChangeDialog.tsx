import { useEffect, useMemo, useState } from 'react'
import {
  ApiError,
  api,
  apiErrorCode,
  formatTime,
  isCapabilitiesChanged,
  skillInUseRefs,
  type NodeSummary,
  type SkillChangeResponse,
  type SkillVersionDetail,
  type SkillVersionSummary,
} from '../api/client'
import {
  capabilityPromptLine,
  defaultInstallVersion,
  filterNodes,
  generationChanged,
  installTarget,
  labelChipsFromNodes,
  labelsMatch,
  nodeLabelMap,
  outcomeDetail,
  outcomeHeadline,
  outcomePillClass,
  resultSummary,
  selectorLabels,
  sortCapabilities,
  toggleLabelChip,
  type LabelChip,
  type NodePick,
  type NodePresence,
} from '../skills/present'
import { Button, ErrorBanner, Loading } from './Ui'

type Action = 'install' | 'rollback' | 'uninstall'
type Step = 'version' | 'nodes' | 'capabilities' | 'summary'

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
  const [nodes, setNodes] = useState<NodeSummary[]>([])
  const [nodesError, setNodesError] = useState<Error | null>(null)
  const [nodesLoading, setNodesLoading] = useState(true)
  const [currentVersion, setCurrentVersion] = useState<
    Record<string, { desired: string | null; actual: string | null }>
  >({})
  const [search, setSearch] = useState('')
  const [presence, setPresence] = useState<NodePresence>('any')
  const [chips, setChips] = useState<LabelChip[]>([])
  const [picked, setPicked] = useState<Record<string, boolean>>({})
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

  useEffect(() => {
    let cancelled = false
    api
      .listNodes()
      .then(async (list) => {
        if (cancelled) return
        setNodes(list)
        setNodesLoading(false)
        const pairs = await Promise.all(
          list.map(async (node) => {
            try {
              const body = await api.getNodeSkills(node.node_id)
              const item = body.items.find((row) => row.name === skillName)
              return [
                node.node_id,
                {
                  desired: item?.desired_version ?? null,
                  actual: item?.actual_version ?? null,
                },
              ] as const
            } catch {
              return [
                node.node_id,
                { desired: null, actual: null },
              ] as const
            }
          }),
        )
        if (cancelled) return
        const next: Record<string, { desired: string | null; actual: string | null }> = {}
        for (const [id, versions] of pairs) next[id] = versions
        setCurrentVersion(next)
      })
      .catch((err: unknown) => {
        if (cancelled) return
        setNodesLoading(false)
        setNodesError(err instanceof Error ? err : new Error(String(err)))
      })
    return () => {
      cancelled = true
    }
  }, [skillName])

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

  const labels = selectorLabels(chips)
  const hasSelector = Object.keys(labels).length > 0
  const handPicked = Object.entries(picked)
    .filter(([, on]) => on)
    .map(([id]) => id)
    .sort()
  const target = installTarget({ handPicked, labels })
  const canTarget = target.node_ids.length > 0 || target.selector != null
  const chipsAvailable = useMemo(() => labelChipsFromNodes(nodes), [nodes])
  const visible = filterNodes(nodes, { search, status: presence, labels: {} })
  const labelHits = hasSelector
    ? nodes.filter((node) => labelsMatch(nodeLabelMap(node.labels), labels))
    : []
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

  function selectAllMatching() {
    const ids = filterNodes(nodes, { search, status: presence, labels }).map(
      (node) => node.node_id,
    )
    setPicked((prev) => {
      const next = { ...prev }
      for (const id of ids) next[id] = true
      return next
    })
  }

  async function submit(forceOverride?: boolean) {
    if (!canTarget || submitting || (action === 'install' && !hashReady)) return
    const useForce = forceOverride ?? force
    setSubmitting(true)
    setSubmitError(null)
    setInUse(null)
    try {
      const response =
        action === 'install'
          ? await api.installSkill(skillName, {
              version,
              node_ids: target.node_ids,
              ...(target.selector ? { selector: target.selector } : {}),
              accepted_capabilities_sha256: displayedHash,
            })
          : action === 'rollback'
            ? await api.rollbackSkill(skillName, {
                node_ids: target.node_ids,
                ...(target.selector ? { selector: target.selector } : {}),
                ...(version ? { to_version: version } : {}),
              })
            : await api.uninstallSkill(skillName, {
                node_ids: target.node_ids,
                ...(target.selector ? { selector: target.selector } : {}),
                force: useForce,
              })
      setCapChanged(null)
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
    (step === 'nodes' && !canTarget) ||
    (step === 'capabilities' && (!reviewed || !hashReady))

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
              <NodePicker
                nodesLoading={nodesLoading}
                nodesError={nodesError}
                chipsAvailable={chipsAvailable}
                chips={chips}
                onToggleChip={(chip) =>
                  setChips((prev) => toggleLabelChip(prev, chip))
                }
                search={search}
                onSearch={setSearch}
                presence={presence}
                onPresence={setPresence}
                visible={visible}
                picked={picked}
                onToggleNode={(id) =>
                  setPicked((prev) => ({ ...prev, [id]: !prev[id] }))
                }
                onSelectMatching={selectAllMatching}
                onClear={() => setPicked({})}
                labels={labels}
                currentVersion={currentVersion}
                handPicked={handPicked}
                labelHits={labelHits.map((node) => node.node_id)}
              />
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
                <p>
                  {action === 'install'
                    ? `Install ${skillName} ${version}.`
                    : action === 'rollback'
                      ? `Roll back ${skillName}${version ? ` to ${version}` : ' to the previous version on each node'}.`
                      : `Uninstall ${skillName}.`}
                </p>
                <dl className="kv">
                  <dt>node_ids</dt>
                  <dd className="mono">
                    {target.node_ids.length > 0 ? target.node_ids.join(', ') : '—'}
                  </dd>
                  <dt>selector.labels</dt>
                  <dd className="mono">
                    {target.selector
                      ? Object.entries(target.selector.labels)
                          .map(([key, value]) => `${key}=${value}`)
                          .join(', ')
                      : 'not sent'}
                  </dd>
                  {action === 'install' ? (
                    <>
                      <dt>accepted hash</dt>
                      <dd className="mono">{displayedHash || '—'}</dd>
                    </>
                  ) : null}
                </dl>
                {hasSelector ? (
                  <p className="muted small">
                    The label filter is sent as <code>selector.labels</code>.
                    Loaded nodes that match:{' '}
                    {labelHits.length > 0
                      ? labelHits.map((node) => node.node_id).join(', ')
                      : 'none'}
                    . Search does not narrow that selector.
                  </p>
                ) : null}
                <p className="muted small">
                  A node the center sees as offline returns{' '}
                  <strong>queued</strong>. A node already on this version
                  returns <strong>already installed</strong>, and its desired
                  generation stays the same.
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
                  disabled={submitting || !canTarget || !hashReady || (action === 'install' && !reviewed)}
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

function NodePicker({
  nodesLoading,
  nodesError,
  chipsAvailable,
  chips,
  onToggleChip,
  search,
  onSearch,
  presence,
  onPresence,
  visible,
  picked,
  onToggleNode,
  onSelectMatching,
  onClear,
  labels,
  currentVersion,
  handPicked,
  labelHits,
}: {
  nodesLoading: boolean
  nodesError: Error | null
  chipsAvailable: LabelChip[]
  chips: LabelChip[]
  onToggleChip: (chip: LabelChip) => void
  search: string
  onSearch: (value: string) => void
  presence: NodePresence
  onPresence: (value: NodePresence) => void
  visible: NodePick[]
  picked: Record<string, boolean>
  onToggleNode: (id: string) => void
  onSelectMatching: () => void
  onClear: () => void
  labels: Record<string, string>
  currentVersion: Record<string, { desired: string | null; actual: string | null }>
  handPicked: string[]
  labelHits: string[]
}) {
  const active = new Set(chips.map((chip) => chip.id))
  const hasSelector = Object.keys(labels).length > 0
  return (
    <div>
      <p className="muted small">
        Hand-picked nodes are sent as <code>node_ids</code>. An active label
        chip is sent as <code>selector.labels</code>, not as a tag.
      </p>
      <label className="field">
        <span className="field-label">Search</span>
        <input
          className="input"
          value={search}
          onChange={(event) => onSearch(event.target.value)}
          placeholder="Node id, hostname, or label"
        />
      </label>
      <label className="field">
        <span className="field-label">Status</span>
        <select
          className="input"
          value={presence}
          onChange={(event) => onPresence(event.target.value as NodePresence)}
        >
          <option value="any">Any</option>
          <option value="seen">Seen</option>
          <option value="never_seen">Never seen</option>
        </select>
      </label>
      <div className="field">
        <span className="field-label">Label filter</span>
        {chipsAvailable.length === 0 ? (
          <p className="muted small">No label pairs on the loaded nodes.</p>
        ) : (
          <div className="chip-row">
            {chipsAvailable.map((chip) => (
              <button
                key={chip.id}
                type="button"
                className={active.has(chip.id) ? 'chip chip-on' : 'chip'}
                aria-pressed={active.has(chip.id)}
                onClick={() => onToggleChip(chip)}
              >
                {chip.key}={chip.value}
              </button>
            ))}
          </div>
        )}
        <p className="muted small">
          One value per label key. <code>role=db</code> includes every node
          whose <code>labels.role</code> is <code>db</code>.
        </p>
      </div>
      <div className="row-gap" style={{ marginBottom: '0.75rem' }}>
        <Button variant="ghost" onClick={onSelectMatching}>
          Select all matching filter
        </Button>
        <Button variant="ghost" onClick={onClear}>
          Clear hand-picked
        </Button>
      </div>
      {nodesLoading ? <Loading label="Loading nodes…" /> : null}
      {nodesError ? <ErrorBanner error={nodesError} /> : null}
      {!nodesLoading && visible.length === 0 ? (
        <p className="muted">No nodes match the search and status filter.</p>
      ) : null}
      {visible.length > 0 ? (
        <div className="table-wrap">
          <table className="data-table">
            <thead>
              <tr>
                <th>Hand-pick</th>
                <th>Node</th>
                <th>Labels</th>
                <th>Status</th>
                <th>Current version</th>
              </tr>
            </thead>
            <tbody>
              {visible.map((node) => {
                const nodeLabels = nodeLabelMap(node.labels)
                const included =
                  hasSelector && labelsMatch(nodeLabels, labels)
                const versions = currentVersion[node.node_id]
                return (
                  <tr key={node.node_id} className={included ? 'row-label' : undefined}>
                    <td>
                      <input
                        type="checkbox"
                        checked={!!picked[node.node_id]}
                        aria-label={`Hand-pick ${node.node_id}`}
                        onChange={() => onToggleNode(node.node_id)}
                      />
                    </td>
                    <td>
                      <div className="mono">{node.node_id}</div>
                      <div className="muted small">{node.hostname || '—'}</div>
                      {included ? (
                        <div className="small">Included by label filter</div>
                      ) : null}
                    </td>
                    <td className="mono small">
                      {Object.entries(nodeLabels)
                        .map(([key, value]) => `${key}=${value}`)
                        .join(' ') || '—'}
                    </td>
                    <td className="nowrap">
                      {node.last_seen_at ? `Seen ${formatTime(node.last_seen_at)}` : 'Never seen'}
                    </td>
                    <td className="mono">
                      {versions === undefined ? (
                        '…'
                      ) : (
                        <>
                          <div>{versions.desired || '—'}</div>
                          {versions.actual && versions.actual !== versions.desired ? (
                            <div className="muted small">actual {versions.actual}</div>
                          ) : null}
                        </>
                      )}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      ) : null}
      <p className="muted small">
        Hand-picked: {handPicked.length > 0 ? handPicked.join(', ') : 'none'}.
        Label filter:{' '}
        {labelHits.length > 0 ? labelHits.join(', ') : hasSelector ? 'none loaded' : 'off'}
        .
      </p>
    </div>
  )
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
