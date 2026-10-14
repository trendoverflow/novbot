import { useState } from 'react'
import { api, type BulkTagResponse } from '../api/client'
import { tagKeyError, tagValueError } from '../tags/selector'
import { Button, ErrorBanner } from './Ui'

type Mode = 'add' | 'remove'

export function TagBulkDialog({
  mode,
  nodeIds,
  onClose,
  onApplied,
}: {
  mode: Mode
  nodeIds: string[]
  onClose: () => void
  onApplied: () => void
}) {
  const [key, setKey] = useState(mode === 'add' ? 'env' : '')
  const [value, setValue] = useState('')
  const [preview, setPreview] = useState<BulkTagResponse | null>(null)
  const [previewKey, setPreviewKey] = useState('')
  const [previewValue, setPreviewValue] = useState('')
  const [prodTyped, setProdTyped] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<Error | null>(null)
  const [applied, setApplied] = useState<BulkTagResponse | null>(null)

  const title = mode === 'add' ? 'Add tag' : 'Remove tag'
  const formMatchesPreview =
    preview !== null &&
    previewKey === key.trim() &&
    (mode === 'remove' || previewValue === value.trim())
  const needsProd = (preview?.prod_node_count ?? 0) > 0
  const prodOk = !needsProd || prodTyped === 'prod'

  async function run(dryRun: boolean) {
    const tagKey = key.trim()
    const tagValue = value.trim()
    const keyError = tagKeyError(tagKey)
    if (keyError) {
      setError(new Error(keyError))
      return
    }
    if (mode === 'add') {
      const valueError = tagValueError(tagValue)
      if (valueError) {
        setError(new Error(valueError))
        return
      }
    }
    if (!dryRun && !formMatchesPreview) {
      setError(new Error('Preview this change before applying it.'))
      return
    }
    if (!dryRun && !prodOk) {
      setError(new Error('Type prod to confirm a change that touches prod nodes.'))
      return
    }
    setBusy(true)
    setError(null)
    try {
      const body = {
        node_ids: nodeIds,
        add: mode === 'add' ? { [tagKey]: tagValue } : {},
        remove: mode === 'remove' ? [tagKey] : [],
        dry_run: dryRun,
        ...(!dryRun && needsProd
          ? { confirm_prod: { node_count: preview?.prod_node_count ?? 0 } }
          : {}),
      }
      const result = await api.bulkTags(body)
      if (dryRun) {
        setPreview(result)
        setPreviewKey(tagKey)
        setPreviewValue(tagValue)
        setProdTyped('')
      } else {
        setApplied(result)
        onApplied()
      }
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="dialog-backdrop">
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby="tag-bulk-title">
        <div className="dialog-head">
          <h2 id="tag-bulk-title">{title}</h2>
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            Close
          </Button>
        </div>
        <p className="muted small">
          {nodeIds.length} selected node{nodeIds.length === 1 ? '' : 's'}. Center tags only.
          A dry run does not write.
        </p>
        {applied ? (
          <BulkSummary result={applied} />
        ) : (
          <>
            <div className="tag-filter">
              <label className="field inline">
                <span className="field-label">Key</span>
                <input
                  className="input"
                  value={key}
                  onChange={(event) => {
                    setKey(event.target.value)
                    setPreview(null)
                  }}
                />
              </label>
              {mode === 'add' ? (
                <label className="field inline">
                  <span className="field-label">Value</span>
                  <input
                    className="input"
                    value={value}
                    onChange={(event) => {
                      setValue(event.target.value)
                      setPreview(null)
                    }}
                  />
                </label>
              ) : null}
            </div>
            {error ? <ErrorBanner error={error} /> : null}
            {preview ? (
              <>
                <BulkSummary result={preview} />
                {needsProd ? (
                  <label className="field">
                    <span className="field-label">
                      Type prod to confirm {preview.prod_node_count} prod node
                      {preview.prod_node_count === 1 ? '' : 's'}
                    </span>
                    <input
                      className="input"
                      value={prodTyped}
                      onChange={(event) => setProdTyped(event.target.value)}
                      autoComplete="off"
                    />
                  </label>
                ) : null}
              </>
            ) : null}
            <div className="dialog-actions">
              <Button variant="ghost" onClick={() => void run(true)} disabled={busy}>
                Preview
              </Button>
              <Button
                onClick={() => void run(false)}
                disabled={busy || !formMatchesPreview || !prodOk}
              >
                Apply
              </Button>
            </div>
          </>
        )}
        {applied && error ? <ErrorBanner error={error} /> : null}
      </div>
    </div>
  )
}

function BulkSummary({ result }: { result: BulkTagResponse }) {
  const counts = result.counts
  return (
    <div>
      <p>
        {result.dry_run ? 'Dry run' : 'Applied'}
        {result.operation_id ? ` · ${result.operation_id}` : ''}. Added {counts.added}, changed{' '}
        {counts.changed}, unchanged {counts.unchanged}, removed {counts.removed}, boot-reported{' '}
        {counts.not_changed_boot_reported}, not found {counts.not_found}, errors {counts.error}.
        {result.prod_node_count > 0
          ? ` Prod nodes: ${result.prod_node_count}.`
          : ''}
      </p>
      <div className="table-wrap">
        <table className="data-table">
          <thead>
            <tr>
              <th>Node</th>
              <th>Key</th>
              <th>Action</th>
              <th>Detail</th>
            </tr>
          </thead>
          <tbody>
            {result.per_node.flatMap((node) =>
              node.results.map((item) => (
                <tr key={`${node.node_id}:${item.key}:${item.action}`}>
                  <td className="mono">{node.node_id}</td>
                  <td className="mono">{item.key || '—'}</td>
                  <td>{item.action}</td>
                  <td className="mono">{itemDetail(item)}</td>
                </tr>
              )),
            )}
          </tbody>
        </table>
      </div>
    </div>
  )
}

function itemDetail(item: BulkTagResponse['per_node'][number]['results'][number]): string {
  if (item.error) return item.error
  if (item.old_value && item.new_value) return `${item.old_value} → ${item.new_value}`
  if (item.new_value) return item.new_value
  if (item.old_value) return item.old_value
  return '—'
}
