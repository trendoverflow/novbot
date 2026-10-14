import { useState } from 'react'
import {
  ApiError,
  api,
  formatTime,
  targetChangedDetail,
  type TargetDispatchResponse,
} from '../api/client'
import { TargetPicker, type PickerState } from '../components/TargetPicker'
import {
  Button,
  ErrorBanner,
  Loading,
  SuccessBanner,
  Toolbar,
} from '../components/Ui'
import { JsonTextArea } from '../components/JsonTextArea'
import { useAsync } from '../hooks/useAsync'

const emptyPick: PickerState = {
  ready: false,
  target: null,
  preview: null,
}

export function DispatchPage() {
  const ops = useAsync(() => api.listDispatchOperations(), [])
  const [specId, setSpecId] = useState('')
  const [paramsText, setParamsText] = useState('{}')
  const [pick, setPick] = useState<PickerState>(emptyPick)
  const [refreshKey, setRefreshKey] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<Error | null>(null)
  const [drift, setDrift] = useState<{ added: string[]; removed: string[] } | null>(
    null,
  )
  const [result, setResult] = useState<TargetDispatchResponse | null>(null)

  async function onDispatch() {
    if (!pick.ready || !pick.target) return
    setBusy(true)
    setError(null)
    setResult(null)
    try {
      const params = JSON.parse(paramsText) as unknown
      const response = await api.dispatchTarget({
        target: pick.target,
        spec_id: specId.trim(),
        params,
        ...(pick.confirmProd ? { confirm_prod: pick.confirmProd } : {}),
      })
      setDrift(null)
      setResult(response)
      ops.reload()
    } catch (err) {
      const changed = targetChangedDetail(err)
      if (changed) {
        setDrift(changed)
        setRefreshKey((n) => n + 1)
      }
      setError(err instanceof Error ? err : new Error(String(err)))
    } finally {
      setBusy(false)
    }
  }

  const paramsOk = (() => {
    try {
      JSON.parse(paramsText)
      return true
    } catch {
      return false
    }
  })()

  return (
    <section>
      <h1 className="page-title">Dispatch</h1>
      <p className="page-lead">
        One-off dispatch to a frozen target. Center previews the match, then{' '}
        <code>POST /v1/dispatch</code> stores that list.
      </p>
      <div className="card form-stack" style={{ maxWidth: '52rem' }}>
        <label className="field">
          <span className="field-label">spec_id</span>
          <input
            className="input"
            value={specId}
            onChange={(event) => setSpecId(event.target.value)}
            placeholder="cpu"
            disabled={busy}
          />
        </label>
        <JsonTextArea
          label="params"
          value={paramsText}
          onChange={setParamsText}
          rows={6}
          disabled={busy}
        />
        <TargetPicker
          onChange={setPick}
          refreshKey={refreshKey}
          disabled={busy}
        />
        {drift ? (
          <div className="alert alert-error" role="alert">
            <strong>409 target_changed</strong>
            <div>
              Added {drift.added.join(', ') || 'none'}. Removed{' '}
              {drift.removed.join(', ') || 'none'}. The preview was refreshed.
              Confirm again after it settles.
            </div>
          </div>
        ) : null}
        {error && !(error instanceof ApiError && error.status === 409) ? (
          <ErrorBanner error={error} />
        ) : null}
        <Toolbar>
          <Button
            onClick={() => void onDispatch()}
            disabled={busy || !pick.ready || !specId.trim() || !paramsOk}
          >
            {busy ? 'Dispatching…' : 'Dispatch'}
          </Button>
        </Toolbar>
      </div>

      {result ? (
        <div className="card" style={{ marginTop: '1rem' }}>
          <SuccessBanner>
            Accepted operation <code>{result.operation_id}</code>. Resolution{' '}
            <code>{result.resolution.id ?? result.resolution.resolution_sha256}</code>
            .
          </SuccessBanner>
          <RunTable rows={result.per_node} warnings={result.warnings} />
        </div>
      ) : null}

      <h2 className="tab-title" style={{ marginTop: '1.5rem' }}>
        Recent operations
      </h2>
      {ops.status === 'loading' ? <Loading /> : null}
      {ops.status === 'error' ? <ErrorBanner error={ops.error} /> : null}
      {ops.status === 'ready' ? (
        ops.data.items.length === 0 ? (
          <p className="muted">No dispatch operations yet.</p>
        ) : (
          <div className="table-wrap">
            <table className="data-table">
              <thead>
                <tr>
                  <th>When</th>
                  <th>Spec</th>
                  <th>Nodes</th>
                  <th>Operation</th>
                </tr>
              </thead>
              <tbody>
                {ops.data.items.map((item) => (
                  <tr key={item.id}>
                    <td>{formatTime(item.created_at)}</td>
                    <td className="mono">{item.spec_id}</td>
                    <td className="mono">{item.resolution.node_ids.join(', ')}</td>
                    <td className="mono">{item.id}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )
      ) : null}
    </section>
  )
}

function RunTable({
  rows,
  warnings,
}: {
  rows: TargetDispatchResponse['per_node']
  warnings: TargetDispatchResponse['warnings']
}) {
  return (
    <div className="table-wrap">
      <table className="data-table">
        <thead>
          <tr>
            <th>Node</th>
            <th>Delivered</th>
            <th>Run</th>
            <th>Warning</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => {
            const warning = warnings.find((item) => item.node_id === row.node_id)
            return (
              <tr key={row.node_id}>
                <td className="mono">{row.node_id}</td>
                <td>{row.delivered}</td>
                <td className="mono">{row.run_id}</td>
                <td>{warning?.code ?? '—'}</td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}
