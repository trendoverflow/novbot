import { useEffect, useMemo, useState } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import { api, formatTime, type NodeSummary } from '../api/client'
import { TagBulkDialog } from '../components/TagBulkDialog'
import { Button, EmptyState, ErrorBanner, Loading, Toolbar } from '../components/Ui'
import { useAsync } from '../hooks/useAsync'
import {
  RECOMMENDED_KEYS,
  VALUE_HINTS,
  addChip,
  canonicalSelector,
  chipLabel,
  parseSelector,
  removeChip,
  tagValueError,
  type FilterChip,
  type FilterOp,
} from '../tags/selector'

const OPS: { id: FilterOp; label: string }[] = [
  { id: 'eq', label: '=' },
  { id: 'neq', label: '!=' },
  { id: 'exists', label: 'has key' },
  { id: 'not', label: 'missing key' },
  { id: 'in', label: 'in' },
  { id: 'notin', label: 'notin' },
]

export function NodesListPage() {
  const [params, setParams] = useSearchParams()
  const selector = params.get('selector') ?? ''
  const parsed = useMemo(() => parseSelector(selector), [selector])
  const state = useAsync(() => api.listNodes(selector || undefined), [selector])
  const [showSystem, setShowSystem] = useState(false)
  const [groupBy, setGroupBy] = useState('')
  const [picked, setPicked] = useState<Record<string, boolean>>({})
  const [draftKey, setDraftKey] = useState('env')
  const [draftOp, setDraftOp] = useState<FilterOp>('eq')
  const [draftValue, setDraftValue] = useState('')
  const [formError, setFormError] = useState<string | null>(null)
  const [bulk, setBulk] = useState<'add' | 'remove' | null>(null)

  useEffect(() => {
    if (!parsed) return
    if (!selector || selector === '*') return
    const canonical = canonicalSelector(parsed)
    if (canonical !== selector) {
      const next = new URLSearchParams(params)
      if (canonical) next.set('selector', canonical)
      else next.delete('selector')
      setParams(next, { replace: true })
    }
  }, [parsed, selector, params, setParams])

  const nodes = state.status === 'ready' ? state.data : []
  const selectedIds = nodes.filter((node) => picked[node.node_id]).map((node) => node.node_id)
  const keyOptions = useMemo(() => {
    const found = new Set<string>(RECOMMENDED_KEYS)
    for (const node of nodes) {
      for (const key of Object.keys(node.tags ?? {})) found.add(key)
    }
    return [...found].sort((a, b) => a.localeCompare(b))
  }, [nodes])

  function writeSelector(next: string) {
    const query = new URLSearchParams(params)
    if (next) query.set('selector', next)
    else query.delete('selector')
    setParams(query, { replace: true })
  }

  function addFilter() {
    const key = draftKey.trim()
    if (!/^[a-z][a-z0-9._-]{0,62}$/.test(key)) {
      setFormError('Filter key must be lowercase and match a-z, digits, ".", "_" or "-".')
      return
    }
    const needsValue = draftOp !== 'exists' && draftOp !== 'not'
    const values =
      draftOp === 'in' || draftOp === 'notin'
        ? draftValue.split(',').map((item) => item.trim()).filter(Boolean)
        : needsValue
          ? [draftValue.trim()]
          : []
    if (needsValue && values.length === 0) {
      setFormError('Enter a value.')
      return
    }
    if (values.some((value) => tagValueError(value))) {
      setFormError('Values must be lowercase and match a-z, digits, ".", "_" or "-".')
      return
    }
    if (!parsed) {
      setFormError('Clear the current selector before adding a chip.')
      return
    }
    const chip: FilterChip = { key, op: draftOp, values }
    writeSelector(canonicalSelector(addChip(parsed, chip)))
    setDraftValue('')
    setFormError(null)
  }

  function toggle(id: string, on: boolean) {
    setPicked((prev) => ({ ...prev, [id]: on }))
  }

  function selectAll(on: boolean) {
    setPicked((prev) => {
      const next = { ...prev }
      for (const node of nodes) next[node.node_id] = on
      return next
    })
  }

  const groups = useMemo(() => groupNodes(nodes, groupBy), [nodes, groupBy])

  return (
    <section>
      <h1 className="page-title">Nodes</h1>
      <p className="page-lead">
        Registered nodes from <code>GET /v1/nodes</code>. Tags are the effective map.
      </p>

      <div className="tag-filter">
        <label className="field inline">
          <span className="field-label">Key</span>
          <input
            className="input"
            list="tag-key-options"
            value={draftKey}
            onChange={(event) => setDraftKey(event.target.value)}
          />
          <datalist id="tag-key-options">
            {keyOptions.map((key) => (
              <option key={key} value={key} />
            ))}
          </datalist>
        </label>
        <label className="field inline">
          <span className="field-label">Match</span>
          <select
            className="input"
            value={draftOp}
            onChange={(event) => setDraftOp(event.target.value as FilterOp)}
          >
            {OPS.map((op) => (
              <option key={op.id} value={op.id}>
                {op.label}
              </option>
            ))}
          </select>
        </label>
        {draftOp === 'exists' || draftOp === 'not' ? null : (
          <label className="field inline">
            <span className="field-label">{draftOp === 'in' || draftOp === 'notin' ? 'Values' : 'Value'}</span>
            <input
              className="input"
              list="tag-value-options"
              value={draftValue}
              placeholder={draftOp === 'in' || draftOp === 'notin' ? 'dev, test' : ''}
              onChange={(event) => setDraftValue(event.target.value)}
            />
            <datalist id="tag-value-options">
              {(VALUE_HINTS[draftKey.trim()] ?? []).map((value) => (
                <option key={value} value={value} />
              ))}
            </datalist>
          </label>
        )}
        <Button onClick={addFilter}>Add filter</Button>
      </div>
      {formError ? <p className="upload-error">{formError}</p> : null}
      {parsed && parsed.length > 0 ? (
        <div className="tag-chips filter-chips">
          {parsed.map((chip, index) => (
            <button
              key={`${chipLabel(chip)}:${index}`}
              type="button"
              className="tag-chip tag-chip-center"
              onClick={() => writeSelector(canonicalSelector(removeChip(parsed, index)))}
              title="Remove condition"
            >
              {chipLabel(chip)} ×
            </button>
          ))}
          <Button variant="ghost" onClick={() => writeSelector('')}>
            Clear
          </Button>
        </div>
      ) : null}
      {parsed === null ? (
        <p className="upload-error">
          Selector <code>{selector}</code> cannot be edited as chips.{' '}
          <button type="button" className="link-button" onClick={() => writeSelector('')}>
            Clear it
          </button>
        </p>
      ) : null}

      <Toolbar>
        <Button variant="ghost" onClick={state.reload} disabled={state.status === 'loading'}>
          Refresh
        </Button>
        <label className="check">
          <input
            type="checkbox"
            checked={showSystem}
            onChange={(event) => setShowSystem(event.target.checked)}
          />
          Show system tags
        </label>
        <label className="field inline">
          <span className="field-label">Group by</span>
          <select className="input" value={groupBy} onChange={(event) => setGroupBy(event.target.value)}>
            <option value="">None</option>
            {keyOptions.map((key) => (
              <option key={key} value={key}>
                {key}
              </option>
            ))}
          </select>
        </label>
        <Button variant="ghost" onClick={() => selectAll(true)} disabled={nodes.length === 0}>
          Select all {nodes.length}
        </Button>
        <Button variant="ghost" onClick={() => selectAll(false)} disabled={selectedIds.length === 0}>
          Clear selection
        </Button>
        <Button onClick={() => setBulk('add')} disabled={selectedIds.length === 0}>
          Add tag
        </Button>
        <Button onClick={() => setBulk('remove')} disabled={selectedIds.length === 0}>
          Remove tag
        </Button>
      </Toolbar>
      {selectedIds.length > 5000 ? (
        <p className="upload-error">At most 5,000 nodes can be changed at once.</p>
      ) : null}

      {state.status === 'loading' ? <Loading /> : null}
      {state.status === 'error' ? <ErrorBanner error={state.error} /> : null}

      {state.status === 'ready' ? (
        nodes.length === 0 ? (
          <div className="card">
            <EmptyState>
              {selector
                ? 'No nodes match this selector.'
                : 'No nodes yet. A node appears after it connects over gRPC, or after PUT /v1/nodes/:id/config.'}
            </EmptyState>
          </div>
        ) : (
          groups.map((group) => (
            <div key={group.id} className="card table-wrap group-block">
              {groupBy ? (
                <h2 className="section-title">
                  {group.label} <span className="muted">({group.nodes.length})</span>
                </h2>
              ) : null}
              <NodeTable
                nodes={group.nodes}
                showSystem={showSystem}
                picked={picked}
                onToggle={toggle}
              />
            </div>
          ))
        )
      ) : null}

      {bulk && selectedIds.length > 0 && selectedIds.length <= 5000 ? (
        <TagBulkDialog
          mode={bulk}
          nodeIds={selectedIds}
          onClose={() => setBulk(null)}
          onApplied={() => state.reload()}
        />
      ) : null}
    </section>
  )
}

function NodeTable({
  nodes,
  showSystem,
  picked,
  onToggle,
}: {
  nodes: NodeSummary[]
  showSystem: boolean
  picked: Record<string, boolean>
  onToggle: (id: string, on: boolean) => void
}) {
  return (
    <table className="data-table">
      <thead>
        <tr>
          <th />
          <th>Node ID</th>
          <th>Hostname</th>
          <th>Version</th>
          <th>Last seen</th>
          <th>Tags</th>
        </tr>
      </thead>
      <tbody>
        {nodes.map((node) => (
          <tr key={node.node_id}>
            <td>
              <input
                type="checkbox"
                checked={Boolean(picked[node.node_id])}
                onChange={(event) => onToggle(node.node_id, event.target.checked)}
                aria-label={`Select ${node.node_id}`}
              />
            </td>
            <td>
              <Link to={`/nodes/${encodeURIComponent(node.node_id)}`}>{node.node_id}</Link>
            </td>
            <td>{node.hostname || '—'}</td>
            <td className="mono">{node.version || '—'}</td>
            <td className="nowrap">{formatTime(node.last_seen_at)}</td>
            <td>
              <TagChips node={node} showSystem={showSystem} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

function TagChips({ node, showSystem }: { node: NodeSummary; showSystem: boolean }) {
  const tags = node.tags ?? {}
  const sources = node.tag_sources ?? {}
  const entries = Object.entries(tags).filter(([key]) => showSystem || sources[key] !== 'system')
  const invalid = node.invalid_tag_count ?? 0
  return (
    <span className="tag-chips">
      {invalid > 0 ? (
        <span className="tag-warn" title={`${invalid} invalid boot-reported tag${invalid === 1 ? '' : 's'}`}>
          ⚠
        </span>
      ) : null}
      {entries.map(([key, value]) => (
        <span key={key} className={chipClass(key, value, sources[key] ?? '')} title={sources[key] ?? ''}>
          {key}={value}
        </span>
      ))}
      {entries.length === 0 && invalid === 0 ? <span className="muted">—</span> : null}
    </span>
  )
}

function chipClass(key: string, value: string, source: string): string {
  if (key === 'env' && value === 'prod') return 'tag-chip tag-chip-prod'
  if (source === 'center') return 'tag-chip tag-chip-center'
  if (source === 'node') return 'tag-chip tag-chip-node'
  return 'tag-chip tag-chip-system'
}

function groupNodes(nodes: NodeSummary[], groupBy: string): { id: string; label: string; nodes: NodeSummary[] }[] {
  if (!groupBy) return [{ id: 'all', label: '', nodes }]
  const buckets = new Map<string, NodeSummary[]>()
  for (const node of nodes) {
    const value = node.tags?.[groupBy] ?? ''
    const list = buckets.get(value) ?? []
    list.push(node)
    buckets.set(value, list)
  }
  return [...buckets.entries()]
    .sort((a, b) => {
      if (!a[0]) return 1
      if (!b[0]) return -1
      return a[0].localeCompare(b[0])
    })
    .map(([value, grouped]) => ({
      id: value || '__none__',
      label: value ? `${groupBy}=${value}` : `${groupBy}: no value`,
      nodes: grouped,
    }))
}
