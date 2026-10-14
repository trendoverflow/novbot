import { useState } from 'react'
import { useParams } from 'react-router-dom'
import {
  api,
  apiErrorCode,
  formatTime,
  type TagRow,
} from '../api/client'
import { Button, EmptyState, ErrorBanner, Loading, SuccessBanner } from '../components/Ui'
import { useAsync } from '../hooks/useAsync'
import {
  RECOMMENDED_KEYS,
  VALUE_HINTS,
  lowercaseSuggestion,
  tagKeyError,
  tagValueError,
} from '../tags/selector'

const KEY_RE = /^[a-z][a-z0-9._-]{0,62}$/

export function NodeTagsTab() {
  const { nodeId = '' } = useParams()
  const state = useAsync(() => api.getNodeTags(nodeId), [nodeId])
  const [key, setKey] = useState('')
  const [value, setValue] = useState('')
  const [lockedKey, setLockedKey] = useState(false)
  const [ifValue, setIfValue] = useState<string | null>(null)
  const [prodTyped, setProdTyped] = useState('')
  const [forceProd, setForceProd] = useState(false)
  const [formError, setFormError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  if (state.status === 'loading') return <Loading />
  if (state.status === 'error') return <ErrorBanner error={state.error} />

  const rows = state.data.tags
  const display = withUnknownPlatform(rows)
  const knownKeys = [...new Set([...RECOMMENDED_KEYS, ...rows.map((row) => row.key)])]
  const prod = countsAsProd(rows)
  const suggestion = lowercaseSuggestion(key, value)
  const pendingProd = prod || (key.trim() === 'env' && value.trim() === 'prod')
  const showProd = pendingProd || forceProd

  function beginEdit(row: TagRow) {
    setKey(row.key)
    setValue(row.value)
    setLockedKey(true)
    setIfValue(row.source === 'center' ? row.value : null)
    setProdTyped('')
    setFormError(null)
    setNotice(null)
  }

  function beginOverride(row: TagRow) {
    setKey(row.key)
    setValue(row.valid ? row.value : '')
    setLockedKey(true)
    setIfValue(null)
    setProdTyped('')
    setFormError(null)
    setNotice('Override stores a center tag. The boot-reported value stays on the host.')
  }

  function resetForm() {
    setKey('')
    setValue('')
    setLockedKey(false)
    setIfValue(null)
    setProdTyped('')
  }

  async function save() {
    const tagKey = key.trim()
    const tagValue = value.trim()
    const keyError = tagKeyError(tagKey)
    if (keyError) {
      setFormError(keyError)
      return
    }
    const valueError = tagValueError(tagValue)
    if (valueError) {
      setFormError(valueError)
      return
    }
    if (showProd && prodTyped !== 'prod') {
      setFormError('Type prod to confirm a change on a prod node, or a change that sets env=prod.')
      return
    }
    setBusy(true)
    setFormError(null)
    try {
      await api.putNodeTag(nodeId, tagKey, {
        value: tagValue,
        ...(ifValue != null ? { if_value: ifValue } : {}),
        ...(showProd ? { confirm_prod: { node_count: 1 } } : {}),
      })
      setNotice(`Saved center tag ${tagKey}=${tagValue}.`)
      resetForm()
      state.reload()
    } catch (err) {
      if (apiErrorCode(err) === 'prod_confirmation_required') setForceProd(true)
      setFormError(saveError(err))
    } finally {
      setBusy(false)
    }
  }

  async function remove(row: TagRow) {
    const needsProd =
      prod ||
      forceProd ||
      (row.key === 'env' &&
        (row.value === 'prod' || (row.node_value ?? '').toLowerCase() === 'prod'))
    if (needsProd && prodTyped !== 'prod') {
      setKey(row.key)
      setValue(row.value)
      setLockedKey(true)
      setIfValue(row.value)
      setForceProd(true)
      setFormError('Type prod to confirm removing a tag on a prod node, or removing env=prod.')
      return
    }
    setBusy(true)
    setFormError(null)
    try {
      await api.deleteNodeTag(nodeId, row.key, needsProd ? 1 : undefined)
      setNotice(`Removed center tag ${row.key}.`)
      resetForm()
      state.reload()
    } catch (err) {
      if (apiErrorCode(err) === 'prod_confirmation_required') setForceProd(true)
      setFormError(saveError(err))
    } finally {
      setBusy(false)
    }
  }

  const centerKeys = new Set(rows.filter((row) => row.source === 'center').map((row) => row.key))

  return (
    <div>
      <div className="tab-head">
        <h2 className="tab-title">Tags</h2>
        <Button variant="ghost" onClick={state.reload}>
          Refresh
        </Button>
      </div>
      <p className="muted small">
        Center tags are edited here. Node tags come from <code>NOVBOT_NODE_TAGS</code>,{' '}
        <code>NOVBOT_NODE_LABELS</code>, <code>--tags</code>, or <code>--labels</code> on the host.
        System tags are read-only.
      </p>

      <div className="tag-filter">
        <label className="field inline">
          <span className="field-label">Key</span>
          <input
            className="input"
            list="node-tag-keys"
            value={key}
            disabled={lockedKey}
            onChange={(event) => setKey(event.target.value)}
          />
          <datalist id="node-tag-keys">
            {knownKeys.map((item) => (
              <option key={item} value={item} />
            ))}
          </datalist>
        </label>
        <label className="field inline">
          <span className="field-label">Value</span>
          <input
            className="input"
            list="node-tag-values"
            value={value}
            onChange={(event) => setValue(event.target.value)}
          />
          <datalist id="node-tag-values">
            {(VALUE_HINTS[key.trim()] ?? []).map((item) => (
              <option key={item} value={item} />
            ))}
          </datalist>
        </label>
        <Button onClick={() => void save()} disabled={busy}>
          {lockedKey && ifValue != null ? 'Save' : lockedKey ? 'Override' : 'Add tag'}
        </Button>
        {lockedKey ? (
          <Button variant="ghost" onClick={resetForm} disabled={busy}>
            Cancel
          </Button>
        ) : null}
      </div>
      {suggestion ? (
        <p className="muted small">
          Uppercase is invalid.{' '}
          <button
            type="button"
            className="link-button"
            onClick={() => {
              setKey(suggestion.key)
              setValue(suggestion.value)
              setFormError(null)
            }}
          >
            Use {suggestion.key}={suggestion.value}
          </button>
        </p>
      ) : null}
      {showProd ? (
        <label className="field">
          <span className="field-label">Type prod to confirm</span>
          <input
            className="input"
            value={prodTyped}
            autoComplete="off"
            onChange={(event) => setProdTyped(event.target.value)}
          />
        </label>
      ) : null}
      {formError ? <p className="upload-error">{formError}</p> : null}
      {notice ? <SuccessBanner>{notice}</SuccessBanner> : null}

      {display.length === 0 ? (
        <EmptyState>No tags on this node yet.</EmptyState>
      ) : (
        <div className="table-wrap">
          <table className="data-table">
            <thead>
              <tr>
                <th>Key</th>
                <th>Effective value</th>
                <th>Source</th>
                <th>Node-reported</th>
                <th>Last changed</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {display.map((row) => (
                <tr key={`${row.source}:${row.key}:${row.synthetic ? 'synth' : row.value}`}>
                  <td className="mono">{row.key}</td>
                  <td>{effectiveText(row)}</td>
                  <td>{sourceText(row)}</td>
                  <td className="mono">{row.node_value ?? '—'}</td>
                  <td>{changedText(row)}</td>
                  <td>
                    <RowActions
                      row={row}
                      centerKeys={centerKeys}
                      busy={busy}
                      onEdit={beginEdit}
                      onOverride={beginOverride}
                      onRemove={(item) => void remove(item)}
                    />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}

type DisplayRow = TagRow & { synthetic: boolean }

function withUnknownPlatform(rows: TagRow[]): DisplayRow[] {
  const display: DisplayRow[] = rows.map((row) => ({ ...row, synthetic: false }))
  for (const key of ['sys.os', 'sys.arch']) {
    if (!rows.some((row) => row.key === key)) {
      display.push({
        key,
        value: '',
        source: 'system',
        effective: false,
        overridden: false,
        node_value: null,
        valid: true,
        invalid_reason: null,
        updated_at: '',
        updated_by: '',
        synthetic: true,
      })
    }
  }
  display.sort((a, b) => a.key.localeCompare(b.key) || a.source.localeCompare(b.source))
  return display
}

function countsAsProd(rows: TagRow[]): boolean {
  if (rows.some((row) => row.effective && row.key === 'env' && row.value === 'prod')) return true
  return rows.some(
    (row) =>
      row.source === 'node' &&
      !row.valid &&
      row.key.toLowerCase() === 'env' &&
      row.value.toLowerCase() === 'prod',
  )
}

function effectiveText(row: DisplayRow): string {
  if (row.synthetic) return 'unknown (node too old)'
  if (!row.valid) return `— (invalid: ${row.invalid_reason ?? 'invalid'}, reported ${row.value})`
  if (!row.effective) return '—'
  return row.value
}

function sourceText(row: DisplayRow): string {
  if (row.synthetic) return 'system'
  if (!row.valid) return 'node'
  if (row.source === 'center' && row.overridden) {
    return row.node_value === row.value ? 'center (same as node)' : 'center · overridden by center'
  }
  return row.source
}

function changedText(row: DisplayRow): string {
  if (row.synthetic || !row.updated_at) return '—'
  if (row.source === 'node' || row.source === 'system') return 'at boot'
  return `${formatTime(row.updated_at)} by ${row.updated_by || '—'}`
}

function RowActions({
  row,
  centerKeys,
  busy,
  onEdit,
  onOverride,
  onRemove,
}: {
  row: DisplayRow
  centerKeys: Set<string>
  busy: boolean
  onEdit: (row: TagRow) => void
  onOverride: (row: TagRow) => void
  onRemove: (row: TagRow) => void
}) {
  if (row.synthetic || row.source === 'system') return <span className="muted">read-only</span>
  if (row.source === 'center') {
    return (
      <span className="row-gap">
        <button type="button" className="link-button" disabled={busy} onClick={() => onEdit(row)}>
          Edit
        </button>
        <button type="button" className="link-button" disabled={busy} onClick={() => onRemove(row)}>
          Remove
        </button>
      </span>
    )
  }
  if (!row.valid && !KEY_RE.test(row.key)) {
    return <span className="muted">not used in filters</span>
  }
  if (centerKeys.has(row.key)) return <span className="muted">—</span>
  return (
    <button type="button" className="link-button" disabled={busy} onClick={() => onOverride(row)}>
      Override
    </button>
  )
}

function saveError(err: unknown): string {
  if (apiErrorCode(err) === 'prod_confirmation_required') {
    return 'Prod confirmation required. Type prod and save again.'
  }
  return err instanceof Error ? err.message : String(err)
}
