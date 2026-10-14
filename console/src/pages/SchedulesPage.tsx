import { useState } from 'react'
import {
  api,
  formatTime,
  targetChangedDetail,
  type ReResolveResponse,
  type TargetSchedule,
} from '../api/client'
import {
  ProdTypeConfirm,
  TargetPicker,
  type PickerState,
} from '../components/TargetPicker'
import {
  Button,
  ErrorBanner,
  Loading,
  SuccessBanner,
  Toolbar,
} from '../components/Ui'
import { useAsync } from '../hooks/useAsync'

const emptyPick: PickerState = {
  ready: false,
  target: null,
  preview: null,
}

export function SchedulesPage() {
  const list = useAsync(() => api.listTargetSchedules(), [])
  const [specId, setSpecId] = useState('')
  const [interval, setInterval] = useState('60')
  const [cron, setCron] = useState('')
  const [enabled, setEnabled] = useState(true)
  const [pick, setPick] = useState<PickerState>(emptyPick)
  const [refreshKey, setRefreshKey] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<Error | null>(null)
  const [drift, setDrift] = useState<{ added: string[]; removed: string[] } | null>(
    null,
  )
  const [createdId, setCreatedId] = useState<string | null>(null)
  const [selected, setSelected] = useState<string | null>(null)

  async function onCreate() {
    if (!pick.ready || !pick.target) return
    setBusy(true)
    setError(null)
    setCreatedId(null)
    try {
      const intervalNum = interval.trim() ? Number(interval) : undefined
      const response = await api.createTargetSchedule({
        target: pick.target,
        spec_id: specId.trim(),
        ...(intervalNum != null && !Number.isNaN(intervalNum)
          ? { interval_secs: intervalNum }
          : {}),
        ...(cron.trim() ? { cron: cron.trim() } : {}),
        enabled,
        ...(pick.confirmProd ? { confirm_prod: pick.confirmProd } : {}),
      })
      setDrift(null)
      setCreatedId(response.id)
      setSelected(response.id)
      list.reload()
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

  const timingOk = interval.trim() !== '' || cron.trim() !== ''
  const items = list.status === 'ready' ? list.data.items : []
  const current = items.find((item) => item.id === selected) ?? null

  return (
    <section>
      <h1 className="page-title">Schedules</h1>
      <p className="page-lead">
        Create a schedule from a frozen target. Re-resolve compares the live
        match with the stored list. Dynamic follow is not available.
      </p>
      <div className="card form-stack" style={{ maxWidth: '52rem' }}>
        <label className="field">
          <span className="field-label">spec_id</span>
          <input
            className="input"
            value={specId}
            disabled={busy}
            onChange={(event) => setSpecId(event.target.value)}
          />
        </label>
        <div className="row-gap">
          <label className="field">
            <span className="field-label">interval_secs</span>
            <input
              className="input"
              value={interval}
              disabled={busy}
              onChange={(event) => setInterval(event.target.value)}
            />
          </label>
          <label className="field">
            <span className="field-label">cron</span>
            <input
              className="input"
              value={cron}
              disabled={busy}
              placeholder="*/5 * * * *"
              onChange={(event) => setCron(event.target.value)}
            />
          </label>
        </div>
        <label className="check">
          <input
            type="checkbox"
            checked={enabled}
            disabled={busy}
            onChange={(event) => setEnabled(event.target.checked)}
          />
          Enabled
        </label>
        <TargetPicker onChange={setPick} refreshKey={refreshKey} disabled={busy} />
        {drift ? <DriftBanner drift={drift} /> : null}
        {error ? <ErrorBanner error={error} /> : null}
        <Toolbar>
          <Button
            onClick={() => void onCreate()}
            disabled={busy || !pick.ready || !specId.trim() || !timingOk}
          >
            {busy ? 'Creating…' : 'Create schedule'}
          </Button>
        </Toolbar>
        {createdId ? (
          <SuccessBanner>
            Created schedule <code>{createdId}</code>.
          </SuccessBanner>
        ) : null}
      </div>

      <h2 className="tab-title" style={{ marginTop: '1.5rem' }}>
        Stored schedules
      </h2>
      <Toolbar>
        <Button variant="ghost" onClick={list.reload}>
          Refresh
        </Button>
      </Toolbar>
      {list.status === 'loading' ? <Loading /> : null}
      {list.status === 'error' ? <ErrorBanner error={list.error} /> : null}
      {list.status === 'ready' && items.length === 0 ? (
        <p className="muted">No target schedules yet.</p>
      ) : null}
      {items.length > 0 ? (
        <div className="table-wrap">
          <table className="data-table">
            <thead>
              <tr>
                <th>Spec</th>
                <th>Resolved</th>
                <th>Drift</th>
                <th>Nodes</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {items.map((item) => (
                <tr key={item.id}>
                  <td>
                    <div className="mono">{item.spec_id}</div>
                    <div className="muted small">
                      {item.enabled ? 'enabled' : 'disabled'}
                      {item.interval_secs != null
                        ? ` · ${item.interval_secs}s`
                        : ''}
                      {item.cron ? ` · ${item.cron}` : ''}
                    </div>
                  </td>
                  <td>{formatTime(item.resolved_at)}</td>
                  <td className="small">
                    +{item.drift.added.length} / −{item.drift.removed.length}
                  </td>
                  <td className="mono small">{item.node_ids.join(', ') || '—'}</td>
                  <td>
                    <Button
                      variant="ghost"
                      onClick={() => setSelected(item.id)}
                    >
                      {selected === item.id ? 'Selected' : 'Manage'}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}

      {current ? (
        <ScheduleEditor
          schedule={current}
          onChanged={() => list.reload()}
        />
      ) : null}
    </section>
  )
}

function DriftBanner({
  drift,
}: {
  drift: { added: string[]; removed: string[] }
}) {
  return (
    <div className="alert alert-error" role="alert">
      <strong>409 target_changed</strong>
      <div>
        Added {drift.added.join(', ') || 'none'}. Removed{' '}
        {drift.removed.join(', ') || 'none'}. Preview again, then confirm.
      </div>
    </div>
  )
}

function ScheduleEditor({
  schedule,
  onChanged,
}: {
  schedule: TargetSchedule
  onChanged: () => void
}) {
  const [interval, setInterval] = useState(
    schedule.interval_secs != null ? String(schedule.interval_secs) : '',
  )
  const [cron, setCron] = useState(schedule.cron ?? '')
  const [enabled, setEnabled] = useState(schedule.enabled)
  const [typed, setTyped] = useState('')
  const [preview, setPreview] = useState<ReResolveResponse | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<Error | null>(null)
  const [drift, setDrift] = useState<{ added: string[]; removed: string[] } | null>(
    null,
  )
  const [note, setNote] = useState<string | null>(null)

  async function onPatch() {
    setBusy(true)
    setError(null)
    setNote(null)
    try {
      await api.patchTargetSchedule(schedule.id, {
        enabled,
        ...(interval.trim()
          ? { interval_secs: Number(interval) }
          : { clear_interval: true }),
        ...(cron.trim() ? { cron: cron.trim() } : { clear_cron: true }),
      })
      setNote('Timing updated on the current node list.')
      onChanged()
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
    } finally {
      setBusy(false)
    }
  }

  async function onDryRun() {
    setBusy(true)
    setError(null)
    setNote(null)
    setDrift(null)
    try {
      const response = await api.reResolveSchedule(schedule.id, { dry_run: true })
      setPreview(response)
      setTyped('')
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
    } finally {
      setBusy(false)
    }
  }

  async function onApply() {
    if (!preview) return
    setBusy(true)
    setError(null)
    try {
      const response = await api.reResolveSchedule(schedule.id, {
        dry_run: false,
        expected_diff: { added: preview.added, removed: preview.removed },
        ...(preview.prod_node_count > 0
          ? { confirm_prod: { node_count: preview.prod_node_count } }
          : {}),
      })
      setPreview(response)
      setDrift(null)
      setNote('Re-resolved and froze the new list.')
      onChanged()
    } catch (err) {
      const changed = targetChangedDetail(err)
      if (changed) setDrift(changed)
      setError(err instanceof Error ? err : new Error(String(err)))
    } finally {
      setBusy(false)
    }
  }

  async function onDelete() {
    setBusy(true)
    setError(null)
    try {
      await api.deleteTargetSchedule(schedule.id)
      setNote('Schedule deleted.')
      onChanged()
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
    } finally {
      setBusy(false)
    }
  }

  const applyBlocked =
    preview == null ||
    !preview.dry_run ||
    (preview.prod_node_count > 0 && typed !== 'prod')

  return (
    <div className="card form-stack" style={{ marginTop: '1rem', maxWidth: '52rem' }}>
      <h2 className="tab-title">Schedule {schedule.id}</h2>
      <p className="muted small">
        Resolved {formatTime(schedule.resolved_at)}. Drift added{' '}
        {schedule.drift.added.join(', ') || 'none'}, removed{' '}
        {schedule.drift.removed.join(', ') || 'none'}.
      </p>
      <div className="row-gap">
        <label className="field">
          <span className="field-label">interval_secs</span>
          <input
            className="input"
            value={interval}
            disabled={busy}
            onChange={(event) => setInterval(event.target.value)}
          />
        </label>
        <label className="field">
          <span className="field-label">cron</span>
          <input
            className="input"
            value={cron}
            disabled={busy}
            onChange={(event) => setCron(event.target.value)}
          />
        </label>
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={enabled}
          disabled={busy}
          onChange={(event) => setEnabled(event.target.checked)}
        />
        Enabled
      </label>
      <Toolbar>
        <Button onClick={() => void onPatch()} disabled={busy}>
          Save timing
        </Button>
        <Button variant="ghost" onClick={() => void onDryRun()} disabled={busy}>
          Preview re-resolve
        </Button>
        <Button variant="ghost" onClick={() => void onDelete()} disabled={busy}>
          Delete
        </Button>
      </Toolbar>
      {preview ? (
        <div>
          <p className="muted small">
            {preview.dry_run ? 'Dry run' : 'Applied'}. Added{' '}
            {preview.added.join(', ') || 'none'}. Removed{' '}
            {preview.removed.join(', ') || 'none'}.
          </p>
          <ProdTypeConfirm
            prodCount={preview.dry_run ? preview.prod_node_count : 0}
            value={typed}
            onChange={setTyped}
            disabled={busy}
          />
          <Button onClick={() => void onApply()} disabled={busy || applyBlocked}>
            Apply re-resolve
          </Button>
        </div>
      ) : null}
      {drift ? <DriftBanner drift={drift} /> : null}
      {error ? <ErrorBanner error={error} /> : null}
      {note ? <SuccessBanner>{note}</SuccessBanner> : null}
    </div>
  )
}
