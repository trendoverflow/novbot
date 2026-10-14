import { useEffect, useMemo, useRef, useState } from 'react'
import {
  ApiError,
  api,
  type ConfirmProd,
  type NodeSummary,
  type SharedTarget,
  type TargetResolution,
} from '../api/client'
import { ErrorBanner } from './Ui'

export type PickerState = {
  ready: boolean
  target: SharedTarget | null
  confirmProd?: ConfirmProd
  preview: TargetResolution | null
}

type Mode = 'selector' | 'nodes'

function splitIds(raw: string): string[] {
  const seen = new Set<string>()
  const out: string[] = []
  for (const part of raw.split(/[\s,]+/)) {
    const id = part.trim()
    if (!id || seen.has(id)) continue
    seen.add(id)
    out.push(id)
  }
  out.sort()
  return out
}

export function ProdTypeConfirm({
  prodCount,
  value,
  onChange,
  disabled,
}: {
  prodCount: number
  value: string
  onChange: (value: string) => void
  disabled?: boolean
}) {
  if (prodCount <= 0) return null
  return (
    <label className="field">
      <span className="field-label">Production confirmation</span>
      <input
        className="input"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        disabled={disabled}
        autoComplete="off"
        placeholder="prod"
        aria-label="Type prod to confirm production nodes"
      />
      <span className="muted small">
        Type <code>prod</code> to confirm {prodCount} production{' '}
        {prodCount === 1 ? 'node' : 'nodes'}.
      </span>
    </label>
  )
}

export function TargetPicker({
  onChange,
  refreshKey = 0,
  disabled = false,
}: {
  onChange: (state: PickerState) => void
  refreshKey?: number
  disabled?: boolean
}) {
  const [mode, setMode] = useState<Mode>('selector')
  const [selectorText, setSelectorText] = useState('')
  const [picked, setPicked] = useState<Record<string, boolean>>({})
  const [excludeText, setExcludeText] = useState('')
  const [typedProd, setTypedProd] = useState('')
  const [nodes, setNodes] = useState<NodeSummary[]>([])
  const [nodesError, setNodesError] = useState<Error | null>(null)
  const [preview, setPreview] = useState<TargetResolution | null>(null)
  const [error, setError] = useState<Error | null>(null)
  const [pending, setPending] = useState(false)
  const [stable, setStable] = useState(false)
  const onChangeRef = useRef(onChange)
  onChangeRef.current = onChange

  const pickedIds = useMemo(
    () =>
      Object.entries(picked)
        .filter(([, on]) => on)
        .map(([id]) => id)
        .sort(),
    [picked],
  )
  const exclude = useMemo(() => splitIds(excludeText), [excludeText])
  const queryKey = JSON.stringify({
    mode,
    selector: selectorText.trim(),
    pickedIds,
    exclude,
  })

  const query = useMemo((): SharedTarget | null => {
    const extra = exclude.length > 0 ? { exclude_node_ids: exclude } : {}
    if (mode === 'selector') {
      const selector = selectorText.trim()
      if (!selector) return null
      return { selector, ...extra }
    }
    if (pickedIds.length === 0) return null
    return { node_ids: pickedIds, ...extra }
  }, [mode, selectorText, pickedIds, exclude])

  useEffect(() => {
    let cancelled = false
    api
      .listNodes()
      .then((list) => {
        if (!cancelled) setNodes(list)
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setNodesError(err instanceof Error ? err : new Error(String(err)))
        }
      })
    return () => {
      cancelled = true
    }
  }, [])

  useEffect(() => {
    setStable(false)
    if (!query) {
      setPreview(null)
      setError(null)
      setPending(false)
      setStable(true)
      return
    }
    setPending(true)
    let cancelled = false
    const timer = window.setTimeout(() => {
      api
        .resolveTarget({ target: query })
        .then((view) => {
          if (cancelled) return
          setPreview(view)
          setError(null)
          setPending(false)
          setStable(true)
        })
        .catch((err: unknown) => {
          if (cancelled) return
          setPreview(null)
          setError(err instanceof Error ? err : new Error(String(err)))
          setPending(false)
          setStable(true)
        })
    }, 300)
    return () => {
      cancelled = true
      window.clearTimeout(timer)
    }
  }, [queryKey, refreshKey, query])

  const prodCount = preview?.prod_node_count ?? 0
  const prodOk = prodCount === 0 || typedProd === 'prod'
  const ready =
    stable &&
    !pending &&
    error == null &&
    preview != null &&
    preview.count > 0 &&
    prodOk
  const confirmProd =
    prodCount > 0 && typedProd === 'prod'
      ? { node_count: prodCount }
      : undefined
  const target: SharedTarget | null =
    query && preview && stable && error == null
      ? { ...query, expected_node_ids: preview.node_ids }
      : null
  const reported: PickerState = {
    ready,
    target: ready ? target : null,
    confirmProd: ready ? confirmProd : undefined,
    preview,
  }
  const reportedKey = JSON.stringify({
    ready,
    target: reported.target,
    confirmProd: reported.confirmProd ?? null,
    node_ids: preview?.node_ids ?? null,
    sha: preview?.resolution_sha256 ?? null,
  })
  const reportedRef = useRef(reported)
  reportedRef.current = reported

  useEffect(() => {
    onChangeRef.current(reportedRef.current)
  }, [reportedKey])

  return (
    <div>
      <div className="field">
        <span className="field-label">Target</span>
        <div className="row-gap">
          <label className="check">
            <input
              type="radio"
              name="target-mode"
              checked={mode === 'selector'}
              disabled={disabled}
              onChange={() => setMode('selector')}
            />
            Selector
          </label>
          <label className="check">
            <input
              type="radio"
              name="target-mode"
              checked={mode === 'nodes'}
              disabled={disabled}
              onChange={() => setMode('nodes')}
            />
            Nodes
          </label>
        </div>
      </div>

      {mode === 'selector' ? (
        <label className="field">
          <span className="field-label">Selector</span>
          <input
            className="input"
            value={selectorText}
            disabled={disabled}
            placeholder="role=db,env!=prod or *"
            onChange={(event) => setSelectorText(event.target.value)}
          />
        </label>
      ) : (
        <div className="field">
          <span className="field-label">Nodes</span>
          {nodesError ? <ErrorBanner error={nodesError} /> : null}
          {nodes.length === 0 ? (
            <p className="muted small">No nodes loaded.</p>
          ) : (
            <ul className="check-list">
              {nodes.map((node) => (
                <li key={node.node_id}>
                  <label className="check-row">
                    <input
                      type="checkbox"
                      checked={!!picked[node.node_id]}
                      disabled={disabled}
                      onChange={() =>
                        setPicked((prev) => ({
                          ...prev,
                          [node.node_id]: !prev[node.node_id],
                        }))
                      }
                    />
                    <span className="mono">{node.node_id}</span>
                    <span className="muted">{node.hostname || '—'}</span>
                  </label>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      <label className="field">
        <span className="field-label">Exclude node ids</span>
        <input
          className="input"
          value={excludeText}
          disabled={disabled}
          placeholder="optional, comma-separated"
          onChange={(event) => setExcludeText(event.target.value)}
        />
      </label>

      {pending ? <p className="muted small">Resolving target…</p> : null}
      {error ? <ErrorBanner error={error} /> : null}
      {error instanceof ApiError && error.status === 400 ? (
        <p className="muted small">
          Fix the selector or pick nodes. An empty selector is invalid.
        </p>
      ) : null}

      {preview && !pending ? (
        <div>
          <p className="muted small">
            {preview.count} matched, {preview.offline_count} offline
            {preview.prod_node_count > 0
              ? `, ${preview.prod_node_count} production`
              : ''}
            .
          </p>
          {preview.count === 0 ? (
            <p className="muted">No nodes match this target.</p>
          ) : (
            <div className="table-wrap">
              <table className="data-table">
                <thead>
                  <tr>
                    <th>Node</th>
                    <th>Host</th>
                    <th>Status</th>
                    <th>Tags</th>
                  </tr>
                </thead>
                <tbody>
                  {preview.nodes.map((node) => (
                    <tr key={node.node_id}>
                      <td className="mono">{node.node_id}</td>
                      <td>{node.hostname || '—'}</td>
                      <td>
                        <span className={node.online ? 'pill pill-ok' : 'pill'}>
                          {node.online ? 'online' : 'offline'}
                        </span>{' '}
                        {node.prod ? (
                          <span className="pill pill-err">prod</span>
                        ) : null}{' '}
                        {node.registered ? null : (
                          <span className="pill">unregistered</span>
                        )}
                      </td>
                      <td className="mono small">
                        {Object.entries(node.tags)
                          .map(([key, value]) => `${key}=${value}`)
                          .join(' ') || '—'}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      ) : null}

      <ProdTypeConfirm
        prodCount={prodCount}
        value={typedProd}
        onChange={setTypedProd}
        disabled={disabled}
      />
    </div>
  )
}
