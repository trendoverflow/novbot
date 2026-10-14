import { useMemo, useState } from 'react'
import {
  api,
  targetChangedDetail,
  type PushSkillGroupResponse,
  type SkillGroup,
} from '../api/client'
import { TargetPicker, type PickerState } from '../components/TargetPicker'
import {
  Button,
  EmptyState,
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

export function FleetSkillGroupsPage() {
  const groupsState = useAsync(() => api.listSkillGroups(), [])

  const [groupId, setGroupId] = useState('')
  const [pick, setPick] = useState<PickerState>(emptyPick)
  const [refreshKey, setRefreshKey] = useState(0)
  const [drift, setDrift] = useState<{ added: string[]; removed: string[] } | null>(
    null,
  )
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

  async function onPush() {
    setPushError(null)
    setPushResult(null)
    if (!effectiveGroupId) {
      setPushError(new Error('Select a skill group'))
      return
    }
    if (!pick.ready || !pick.target) {
      setPushError(new Error('Wait for a stable target preview'))
      return
    }
    setPushing(true)
    try {
      const res = await api.pushSkillGroup({
        group_id: effectiveGroupId,
        target: pick.target,
        ...(pick.confirmProd ? { confirm_prod: pick.confirmProd } : {}),
      })
      setDrift(null)
      setPushResult(res)
    } catch (e) {
      const changed = targetChangedDetail(e)
      if (changed) {
        setDrift(changed)
        setRefreshKey((n) => n + 1)
      }
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
        <Button variant="ghost" onClick={() => groupsState.reload()}>
          Refresh
        </Button>
      </Toolbar>

      {groupsState.status === 'loading' ? <Loading /> : null}
      {groupsState.status === 'error' ? (
        <ErrorBanner error={groupsState.error} />
      ) : null}

      {groupsState.status === 'ready' ? (
        <div className="card form-stack" style={{ maxWidth: '52rem' }}>
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

              <TargetPicker
                onChange={setPick}
                refreshKey={refreshKey}
                disabled={pushing}
              />
              {drift ? (
                <div className="alert alert-error" role="alert">
                  <strong>409 target_changed</strong>
                  <div>
                    Added {drift.added.join(', ') || 'none'}. Removed{' '}
                    {drift.removed.join(', ') || 'none'}. Preview again, then
                    push.
                  </div>
                </div>
              ) : null}

              <Toolbar>
                <Button onClick={onPush} disabled={pushing || !pick.ready}>
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
