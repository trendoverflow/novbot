import { useEffect, useMemo, useState } from 'react'
import type { FormEvent } from 'react'
import { NavLink, Outlet, useParams } from 'react-router-dom'
import {
  api,
  formatTime,
  parseJsonField,
  type DispatchResponse,
  type NodeConfig,
} from '../api/client'
import { JsonTextArea } from '../components/JsonTextArea'
import { ResultsTable } from '../components/ResultsTable'
import {
  Button,
  EmptyState,
  ErrorBanner,
  Loading,
  SuccessBanner,
  Toolbar,
} from '../components/Ui'
import { useAsync } from '../hooks/useAsync'

const tabClass = ({ isActive }: { isActive: boolean }) =>
  isActive ? 'tab active' : 'tab'

export function NodeDetailPage() {
  const { nodeId = '' } = useParams()

  return (
    <section>
      <h1 className="page-title">Node: {nodeId || '—'}</h1>
      <p className="page-lead">
        Per-node management against <code>/v1/nodes/{'{id}'}/…</code> and
        node-scoped results.
      </p>
      <nav className="tabs">
        <NavLink to="inventory" className={tabClass}>
          Inventory
        </NavLink>
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

export function NodeInventoryTab() {
  const { nodeId = '' } = useParams()
  const state = useAsync(async () => {
    const nodes = await api.listNodes()
    const row = nodes.find((n) => n.node_id === nodeId) ?? null
    let config: NodeConfig | null = null
    try {
      config = await api.getConfig(nodeId)
    } catch {
      config = null
    }
    return { row, config }
  }, [nodeId])

  if (state.status === 'loading') return <Loading />
  if (state.status === 'error') return <ErrorBanner error={state.error} />

  const { row, config } = state.data
  if (!row && !config) {
    return (
      <EmptyState>
        Node <code>{nodeId}</code> not found in <code>GET /v1/nodes</code> and
        has no config yet.
      </EmptyState>
    )
  }

  return (
    <div>
      <div className="tab-head">
        <h2 className="tab-title">Inventory</h2>
        <Button variant="ghost" onClick={state.reload}>
          Refresh
        </Button>
      </div>
      <p className="muted small">
        Read-only node row + config generation (no dedicated inventory API).
      </p>
      <dl className="kv">
        <dt>Node ID</dt>
        <dd className="mono">{nodeId}</dd>
        <dt>Hostname</dt>
        <dd>{row?.hostname || '—'}</dd>
        <dt>Version</dt>
        <dd className="mono">{row?.version || '—'}</dd>
        <dt>Last seen</dt>
        <dd>{formatTime(row?.last_seen_at ?? null)}</dd>
        <dt>Labels</dt>
        <dd>
          <pre className="json-pre inline">
            {JSON.stringify(row?.labels ?? {}, null, 2)}
          </pre>
        </dd>
        <dt>Config generation</dt>
        <dd className="mono">
          {config ? config.config_generation : '— (no config)'}
        </dd>
        <dt>Spec count</dt>
        <dd className="mono">
          {config
            ? Array.isArray(parseJsonField(config.specs_json))
              ? (parseJsonField(config.specs_json) as unknown[]).length
              : '—'
            : '—'}
        </dd>
      </dl>
    </div>
  )
}

export function NodeConfigTab() {
  const { nodeId = '' } = useParams()
  const state = useAsync(() => api.getConfig(nodeId), [nodeId])
  const [specsText, setSpecsText] = useState('[]')
  const [schedulesText, setSchedulesText] = useState('[]')
  const [includeSchedules, setIncludeSchedules] = useState(true)
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState<Error | null>(null)
  const [saved, setSaved] = useState<NodeConfig | null>(null)

  useEffect(() => {
    if (state.status !== 'ready') return
    setSpecsText(prettyJson(parseJsonField(state.data.specs_json)))
    setSchedulesText(prettyJson(parseJsonField(state.data.schedules_json)))
    setSaved(null)
    setSaveError(null)
  }, [state])

  async function onSave() {
    setSaving(true)
    setSaveError(null)
    setSaved(null)
    try {
      const specs = JSON.parse(specsText) as unknown
      const body =
        includeSchedules
          ? { specs, schedules: JSON.parse(schedulesText) as unknown }
          : { specs }
      const cfg = await api.putConfig(nodeId, body)
      setSaved(cfg)
      setSpecsText(prettyJson(parseJsonField(cfg.specs_json)))
      setSchedulesText(prettyJson(parseJsonField(cfg.schedules_json)))
    } catch (err) {
      setSaveError(err instanceof Error ? err : new Error(String(err)))
    } finally {
      setSaving(false)
    }
  }

  if (state.status === 'loading') return <Loading />
  if (state.status === 'error') {
    return (
      <div>
        <ErrorBanner error={state.error} />
        <p className="muted small">
          If the node has never been configured, create it with a PUT below
          after editing JSON (or seed via curl as in the root README).
        </p>
        <ConfigEditor
          specsText={specsText}
          setSpecsText={setSpecsText}
          schedulesText={schedulesText}
          setSchedulesText={setSchedulesText}
          includeSchedules={includeSchedules}
          setIncludeSchedules={setIncludeSchedules}
          generation={null}
          saving={saving}
          saveError={saveError}
          saved={saved}
          onSave={onSave}
          onReload={state.reload}
        />
      </div>
    )
  }

  return (
    <ConfigEditor
      specsText={specsText}
      setSpecsText={setSpecsText}
      schedulesText={schedulesText}
      setSchedulesText={setSchedulesText}
      includeSchedules={includeSchedules}
      setIncludeSchedules={setIncludeSchedules}
      generation={state.data.config_generation}
      saving={saving}
      saveError={saveError}
      saved={saved}
      onSave={onSave}
      onReload={state.reload}
    />
  )
}

function ConfigEditor(props: {
  specsText: string
  setSpecsText: (v: string) => void
  schedulesText: string
  setSchedulesText: (v: string) => void
  includeSchedules: boolean
  setIncludeSchedules: (v: boolean) => void
  generation: number | null
  saving: boolean
  saveError: Error | null
  saved: NodeConfig | null
  onSave: () => void
  onReload: () => void
}) {
  return (
    <div>
      <div className="tab-head">
        <h2 className="tab-title">Config</h2>
        <div className="row-gap">
          {props.generation != null ? (
            <span className="muted small">
              generation <code>{props.generation}</code>
            </span>
          ) : null}
          <Button variant="ghost" onClick={props.onReload} disabled={props.saving}>
            Reload
          </Button>
          <Button onClick={props.onSave} disabled={props.saving}>
            {props.saving ? 'Saving…' : 'Save PUT'}
          </Button>
        </div>
      </div>
      <p className="muted small">
        <code>PUT /v1/nodes/:id/config</code> body uses parsed JSON{' '}
        <code>specs</code> (and optional <code>schedules</code>), not the
        stored <code>*_json</code> string fields.
      </p>
      <label className="check">
        <input
          type="checkbox"
          checked={props.includeSchedules}
          onChange={(e) => props.setIncludeSchedules(e.target.checked)}
        />
        Include schedules in PUT (omit to leave schedules unchanged on center)
      </label>
      <div className="editor-grid">
        <JsonTextArea
          label="specs"
          value={props.specsText}
          onChange={props.setSpecsText}
          disabled={props.saving}
        />
        {props.includeSchedules ? (
          <JsonTextArea
            label="schedules"
            value={props.schedulesText}
            onChange={props.setSchedulesText}
            disabled={props.saving}
          />
        ) : null}
      </div>
      {props.saveError ? <ErrorBanner error={props.saveError} /> : null}
      {props.saved ? (
        <SuccessBanner>
          Saved. New generation{' '}
          <code>{props.saved.config_generation}</code>.
        </SuccessBanner>
      ) : null}
    </div>
  )
}

export function NodeSchedulesTab() {
  const { nodeId = '' } = useParams()
  const state = useAsync(() => api.getSchedules(nodeId), [nodeId])
  const [text, setText] = useState('[]')
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState<Error | null>(null)
  const [savedGen, setSavedGen] = useState<number | null>(null)

  useEffect(() => {
    if (state.status !== 'ready') return
    setText(prettyJson(state.data.schedules ?? []))
    setSavedGen(null)
    setSaveError(null)
  }, [state])

  async function onSave() {
    setSaving(true)
    setSaveError(null)
    setSavedGen(null)
    try {
      const schedules = JSON.parse(text) as unknown
      const cfg = await api.putSchedules(nodeId, { schedules })
      setSavedGen(cfg.config_generation)
      setText(prettyJson(parseJsonField(cfg.schedules_json)))
    } catch (err) {
      setSaveError(err instanceof Error ? err : new Error(String(err)))
    } finally {
      setSaving(false)
    }
  }

  if (state.status === 'loading') return <Loading />
  if (state.status === 'error') return <ErrorBanner error={state.error} />

  return (
    <div>
      <div className="tab-head">
        <h2 className="tab-title">Schedules</h2>
        <div className="row-gap">
          <span className="muted small">
            generation <code>{state.data.config_generation}</code>
          </span>
          <Button variant="ghost" onClick={state.reload} disabled={saving}>
            Reload
          </Button>
          <Button onClick={onSave} disabled={saving}>
            {saving ? 'Saving…' : 'Save PUT'}
          </Button>
        </div>
      </div>
      <p className="muted small">
        <code>GET/PUT /v1/nodes/:id/schedules</code> — PUT body{' '}
        <code>{'{ schedules }'}</code>.
      </p>
      <JsonTextArea
        label="schedules"
        value={text}
        onChange={setText}
        disabled={saving}
        rows={18}
      />
      {saveError ? <ErrorBanner error={saveError} /> : null}
      {savedGen != null ? (
        <SuccessBanner>
          Schedules saved. New generation <code>{savedGen}</code>.
        </SuccessBanner>
      ) : null}
    </div>
  )
}

export function NodeDispatchTab() {
  const { nodeId = '' } = useParams()
  const configState = useAsync(() => api.getConfig(nodeId), [nodeId])
  const [specId, setSpecId] = useState('')
  const [paramsText, setParamsText] = useState('{}')
  const [runId, setRunId] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<Error | null>(null)
  const [result, setResult] = useState<DispatchResponse | null>(null)

  const specOptions = useMemo(() => {
    if (configState.status !== 'ready') return [] as string[]
    const specs = parseJsonField(configState.data.specs_json)
    if (!Array.isArray(specs)) return []
    return specs
      .map((s) =>
        s && typeof s === 'object' && 'id' in s
          ? String((s as { id: unknown }).id)
          : '',
      )
      .filter(Boolean)
  }, [configState])

  useEffect(() => {
    if (!specId && specOptions.length > 0) setSpecId(specOptions[0])
  }, [specId, specOptions])

  async function onSubmit(e: FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError(null)
    setResult(null)
    try {
      const params = JSON.parse(paramsText) as unknown
      const body = {
        spec_id: specId.trim(),
        params,
        ...(runId.trim() ? { run_id: runId.trim() } : {}),
      }
      const res = await api.dispatch(nodeId, body)
      setResult(res)
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div>
      <div className="tab-head">
        <h2 className="tab-title">Dispatch</h2>
        <Button variant="ghost" onClick={configState.reload}>
          Reload specs
        </Button>
      </div>
      <p className="muted small">
        <code>POST /v1/nodes/:id/dispatch</code> — returns 202 with live or
        queued delivery.
      </p>

      {configState.status === 'loading' ? <Loading label="Loading config…" /> : null}
      {configState.status === 'error' ? (
        <ErrorBanner error={configState.error} />
      ) : null}

      <form className="form-stack" onSubmit={onSubmit}>
        <label className="field">
          <span className="field-label">spec_id</span>
          {specOptions.length > 0 ? (
            <select
              className="input"
              value={specId}
              onChange={(e) => setSpecId(e.target.value)}
              disabled={busy}
            >
              {specOptions.map((id) => (
                <option key={id} value={id}>
                  {id}
                </option>
              ))}
            </select>
          ) : (
            <input
              className="input"
              value={specId}
              onChange={(e) => setSpecId(e.target.value)}
              required
              disabled={busy}
              placeholder="cpu"
            />
          )}
        </label>
        <label className="field">
          <span className="field-label">run_id (optional)</span>
          <input
            className="input"
            value={runId}
            onChange={(e) => setRunId(e.target.value)}
            disabled={busy}
            placeholder="auto UUID if empty"
          />
        </label>
        <JsonTextArea
          label="params"
          value={paramsText}
          onChange={setParamsText}
          rows={8}
          disabled={busy}
        />
        <Toolbar>
          <Button type="submit" disabled={busy || !specId.trim()}>
            {busy ? 'Dispatching…' : 'Dispatch'}
          </Button>
        </Toolbar>
      </form>

      {error ? <ErrorBanner error={error} /> : null}
      {result ? (
        <SuccessBanner>
          Accepted. run_id=<code>{result.run_id}</code> delivered=
          <code>{result.delivered}</code>
        </SuccessBanner>
      ) : null}
    </div>
  )
}

export function NodeResultsTab() {
  const { nodeId = '' } = useParams()
  const [limit, setLimit] = useState(50)
  const state = useAsync(
    () => api.listResults({ node_id: nodeId, limit }),
    [nodeId, limit],
  )

  return (
    <div>
      <div className="tab-head">
        <h2 className="tab-title">Results</h2>
        <div className="row-gap">
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
          <Button variant="ghost" onClick={state.reload}>
            Refresh
          </Button>
        </div>
      </div>
      <p className="muted small">
        <code>
          GET /v1/results?node_id={nodeId}&limit={limit}
        </code>
      </p>
      {state.status === 'loading' ? <Loading /> : null}
      {state.status === 'error' ? <ErrorBanner error={state.error} /> : null}
      {state.status === 'ready' ? (
        <ResultsTable rows={state.data} showNode={false} />
      ) : null}
    </div>
  )
}

function prettyJson(v: unknown): string {
  try {
    return JSON.stringify(v, null, 2)
  } catch {
    return String(v)
  }
}

