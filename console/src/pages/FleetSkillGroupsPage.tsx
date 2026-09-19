import { useMemo, useState } from 'react'
import {
  api,
  type PushSkillGroupResponse,
  type SkillGroup,
} from '../api/client'
import {
  Button,
  EmptyState,
  ErrorBanner,
  Loading,
  SuccessBanner,
  Toolbar,
} from '../components/Ui'
import { useAsync } from '../hooks/useAsync'

export function FleetSkillGroupsPage() {
  const groupsState = useAsync(() => api.listSkillGroups(), [])
  const nodesState = useAsync(() => api.listNodes(), [])

  const [groupId, setGroupId] = useState('')
  const [selected, setSelected] = useState<Record<string, boolean>>({})
  const [pushing, setPushing] = useState(false)
  const [pushError, setPushError] = useState<Error | null>(null)
  const [pushResult, setPushResult] = useState<PushSkillGroupResponse | null>(
    null,
  )

  const groups: SkillGroup[] =
    groupsState.status === 'ready' ? groupsState.data.groups : []

  const activeGroup = useMemo(
    () => groups.find((g) => g.id === groupId) ?? groups[0],
    [groups, groupId],
  )

  const effectiveGroupId = activeGroup?.id ?? ''

  function toggleNode(id: string) {
    setSelected((prev) => ({ ...prev, [id]: !prev[id] }))
  }

  function selectAll(ids: string[]) {
    const next: Record<string, boolean> = {}
    for (const id of ids) next[id] = true
    setSelected(next)
  }

  async function onPush() {
    setPushError(null)
    setPushResult(null)
    const nodeIds = Object.entries(selected)
      .filter(([, on]) => on)
      .map(([id]) => id)
    if (!effectiveGroupId) {
      setPushError(new Error('Select a skill group'))
      return
    }
    if (nodeIds.length === 0) {
      setPushError(new Error('Select at least one node'))
      return
    }
    setPushing(true)
    try {
      const res = await api.pushSkillGroup({
        group_id: effectiveGroupId,
        node_ids: nodeIds,
      })
      setPushResult(res)
    } catch (e) {
      setPushError(e instanceof Error ? e : new Error(String(e)))
    } finally {
      setPushing(false)
    }
  }

  const okCount =
    pushResult?.results.filter((r) => r.status === 'ok').length ?? 0
  const errCount =
    pushResult?.results.filter((r) => r.status === 'error').length ?? 0

  return (
    <section>
      <h1 className="page-title">Fleet · Skill groups</h1>
      <p className="page-lead">
        Select a skill group and node set, then push. Center resolves each
        skill to a matching node spec (or uses the skill name as{' '}
        <code>spec_id</code>) and dispatches.
      </p>

      <Toolbar>
        <Button
          variant="ghost"
          onClick={() => {
            groupsState.reload()
            nodesState.reload()
          }}
        >
          Refresh
        </Button>
      </Toolbar>

      {groupsState.status === 'loading' || nodesState.status === 'loading' ? (
        <Loading />
      ) : null}
      {groupsState.status === 'error' ? (
        <ErrorBanner error={groupsState.error} />
      ) : null}
      {nodesState.status === 'error' ? (
        <ErrorBanner error={nodesState.error} />
      ) : null}

      {groupsState.status === 'ready' && nodesState.status === 'ready' ? (
        <div className="card form-stack" style={{ maxWidth: '48rem' }}>
          {groups.length === 0 ? (
            <EmptyState>No skill groups from center.</EmptyState>
          ) : (
            <>
              <label className="field">
                <span className="field-label">Skill group</span>
                <select
                  className="input"
                  value={effectiveGroupId}
                  onChange={(e) => setGroupId(e.target.value)}
                >
                  {groups.map((g) => (
                    <option key={g.id} value={g.id}>
                      {g.name} ({g.id})
                    </option>
                  ))}
                </select>
              </label>
              {activeGroup ? (
                <p className="muted">
                  {activeGroup.description ?? ''} Skills:{' '}
                  <code>{activeGroup.skills.join(', ')}</code>
                </p>
              ) : null}

              <div className="field">
                <span className="field-label">Nodes</span>
                {nodesState.data.length === 0 ? (
                  <EmptyState>
                    No nodes registered. Configure a node first.
                  </EmptyState>
                ) : (
                  <>
                    <Toolbar>
                      <Button
                        variant="ghost"
                        onClick={() =>
                          selectAll(nodesState.data.map((n) => n.node_id))
                        }
                      >
                        Select all
                      </Button>
                      <Button
                        variant="ghost"
                        onClick={() => setSelected({})}
                      >
                        Clear
                      </Button>
                    </Toolbar>
                    <ul className="check-list">
                      {nodesState.data.map((n) => (
                        <li key={n.node_id}>
                          <label className="check-row">
                            <input
                              type="checkbox"
                              checked={!!selected[n.node_id]}
                              onChange={() => toggleNode(n.node_id)}
                            />
                            <span className="mono">{n.node_id}</span>
                            <span className="muted">
                              {n.hostname || '—'}
                            </span>
                          </label>
                        </li>
                      ))}
                    </ul>
                  </>
                )}
              </div>

              <Toolbar>
                <Button onClick={onPush} disabled={pushing}>
                  {pushing ? 'Pushing…' : 'Push skill group'}
                </Button>
              </Toolbar>
            </>
          )}
        </div>
      ) : null}

      {pushError ? <ErrorBanner error={pushError} /> : null}
      {pushResult ? (
        <div className="card" style={{ marginTop: '1rem' }}>
          <SuccessBanner>
            Push accepted for group <code>{pushResult.group_id}</code> —{' '}
            {okCount} ok, {errCount} error
            {errCount ? '' : '.'}
          </SuccessBanner>
          <div className="table-wrap">
            <table className="data-table">
              <thead>
                <tr>
                  <th>Node</th>
                  <th>Skill</th>
                  <th>Spec</th>
                  <th>Status</th>
                  <th>Delivered</th>
                  <th>Run / error</th>
                </tr>
              </thead>
              <tbody>
                {pushResult.results.map((r, i) => (
                  <tr key={`${r.node_id}-${r.skill ?? ''}-${i}`}>
                    <td className="mono">{r.node_id}</td>
                    <td>{r.skill ?? '—'}</td>
                    <td className="mono">{r.spec_id ?? '—'}</td>
                    <td>{r.status}</td>
                    <td>{r.delivered ?? '—'}</td>
                    <td className="mono">
                      {r.error ?? r.run_id ?? '—'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      ) : null}
    </section>
  )
}
