/**
 * NovBot center HTTP API client.
 *
 * Default: relative `/v1` (Vite proxy / same-origin production).
 * Settings may set an absolute base URL (http/https) and Bearer token.
 * Shapes match crates/novbot-center/src/http.rs + db.rs.
 */

import { loadSettings } from './settings'

export class ApiError extends Error {
  readonly status: number
  readonly body: unknown

  constructor(status: number, body: unknown, message?: string) {
    super(message ?? apiErrorMessage(status, body))
    this.name = 'ApiError'
    this.status = status
    this.body = body
  }
}

function apiErrorMessage(status: number, body: unknown): string {
  if (body && typeof body === 'object' && 'error' in body) {
    const err = (body as { error?: unknown }).error
    if (typeof err === 'string' && err.trim()) return err
  }
  return `API ${status}`
}

function v1Base(): string {
  const base = loadSettings().baseUrl.trim().replace(/\/+$/, '')
  if (!base) return '/v1'
  return `${base}/v1`
}

function healthPath(): string {
  const base = loadSettings().baseUrl.trim().replace(/\/+$/, '')
  if (!base) return '/health'
  return `${base}/health`
}

function authHeaders(): Record<string, string> {
  const token = loadSettings().token.trim()
  return token ? { Authorization: `Bearer ${token}` } : {}
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const url = path.startsWith('http') ? path : `${v1Base()}${path}`
  const res = await fetch(url, {
    ...init,
    headers: {
      Accept: 'application/json',
      ...(init?.body ? { 'Content-Type': 'application/json' } : {}),
      ...authHeaders(),
      ...init?.headers,
    },
  })

  const text = await res.text()
  let body: unknown = null
  if (text) {
    try {
      body = JSON.parse(text) as unknown
    } catch {
      body = text
    }
  }

  if (!res.ok) {
    throw new ApiError(res.status, body)
  }
  return body as T
}

async function requestHealth<T>(init?: RequestInit): Promise<T> {
  const res = await fetch(healthPath(), {
    ...init,
    headers: {
      Accept: 'application/json',
      ...authHeaders(),
      ...init?.headers,
    },
  })
  const text = await res.text()
  let body: unknown = null
  if (text) {
    try {
      body = JSON.parse(text) as unknown
    } catch {
      body = text
    }
  }
  if (!res.ok) {
    throw new ApiError(res.status, body)
  }
  return body as T
}

/** GET /v1/nodes — NodeRow */
export type NodeSummary = {
  node_id: string
  hostname: string
  version: string
  labels: unknown
  last_seen_at: string | null
}

/** GET/PUT /v1/nodes/:id/config — NodeConfig (JSON strings for specs/schedules) */
export type NodeConfig = {
  node_id: string
  config_generation: number
  specs_json: string
  schedules_json: string
}

export type PutConfigBody = {
  specs: unknown
  schedules?: unknown
}

/** GET /v1/nodes/:id/schedules */
export type SchedulesResponse = {
  node_id: string
  config_generation: number
  schedules: unknown
}

export type PutSchedulesBody = {
  schedules: unknown
}

export type DispatchBody = {
  spec_id: string
  params?: unknown
  run_id?: string
}

export type DispatchResponse = {
  accepted: boolean
  node_id: string
  run_id: string
  spec_id: string
  delivered: 'live' | 'queued' | string
}

/** GET /v1/results — ResultRow */
export type ResultRow = {
  id: number
  node_id: string
  run_id: string
  spec_id: string
  status: string
  payload: unknown
  observed_at: string
  received_at: string
}

export type HealthResponse = {
  ok: boolean
  service?: string
}

export type ListResultsQuery = {
  node_id?: string
  limit?: number
}

export type SkillGroup = {
  id: string
  name: string
  description?: string
  skills: string[]
}

export type SkillGroupsResponse = {
  groups: SkillGroup[]
  note?: string
}

export type PushSkillGroupBody = {
  group_id: string
  node_ids: string[]
}

export type PushSkillGroupResultRow = {
  node_id: string
  skill?: string | null
  spec_id?: string
  run_id?: string
  status: string
  delivered?: string
  error?: string
}

export type PushSkillGroupResponse = {
  accepted: boolean
  group_id: string
  group_name?: string
  skills?: string[]
  results: PushSkillGroupResultRow[]
}

export type TokensStatus = {
  auth_required: boolean
  stub?: boolean
  note?: string
}

export type CreateTokenResponse = {
  token: string
  stub?: boolean
  note?: string
}

/** One declared grant on a Hub skill version. */
export type SkillCapability = {
  grant: string
  name: string
  scope: string | null
  risk: string
  description: string
  reason: string | null
}

/** `items[]` on GET /v1/skills. Built-ins have an empty latest_version. */
export type HubSkillItem = {
  name: string
  source: string
  display_name: string
  description: string
  latest_version: string
  sha256: string
  capabilities: SkillCapability[]
}

export type SkillsListResponse = {
  skills: string[]
  items: HubSkillItem[]
  next_page_token: string | null
  note?: string
}

export type SkillVersionSummary = {
  version: string
  sha256: string
  signature_status: string
  status: string
}

export type SkillDetail = {
  name: string
  source: string
  display_name: string
  description: string
  publisher: string | null
  latest_version: string | null
  sha256: string | null
  capabilities: SkillCapability[]
  versions: SkillVersionSummary[]
}

export type SkillVersionDetail = {
  name: string
  version: string
  sha256: string
  size_bytes: number
  content_type: string
  abi: string
  signature_status: string
  status: string
  capabilities: SkillCapability[]
  capabilities_sha256: string
  lint_warnings: string[]
  display_name: string
  description: string
  publisher: string | null
  min_node_version: string | null
  platforms: string[]
}

export type SkillPublishResponse = {
  name: string
  version: string
  sha256: string
  capabilities: SkillCapability[]
  capabilities_sha256: string
  signature_status: string
  lint_warnings: string[]
}

/** Hand-picked nodes plus an optional label map. Never a tags field. */
export type SkillSelector = {
  labels: Record<string, string>
}

export type InstallSkillBody = {
  version: string
  node_ids: string[]
  selector?: SkillSelector
  accepted_capabilities_sha256: string
}

export type RollbackSkillBody = {
  node_ids: string[]
  selector?: SkillSelector
  to_version?: string
}

export type UninstallSkillBody = {
  node_ids: string[]
  selector?: SkillSelector
  force: boolean
}

export type PerNodeOutcome = {
  node_id: string
  outcome: string
  generation: number
}

export type SkillChangeResponse = {
  operation_id: string | null
  dry_run: boolean
  per_node: PerNodeOutcome[]
}

export type NodeSkillItem = {
  name: string
  source: string
  state: string
  desired_version: string | null
  actual_version: string | null
}

export type NodeSkillsResponse = {
  node_id: string
  generation: number
  applied_generation: number
  items: NodeSkillItem[]
}

export const api = {
  health: () => requestHealth<HealthResponse>(),

  listNodes: () => request<NodeSummary[]>('/nodes'),

  getConfig: (nodeId: string) =>
    request<NodeConfig>(`/nodes/${encodeURIComponent(nodeId)}/config`),

  putConfig: (nodeId: string, body: PutConfigBody) =>
    request<NodeConfig>(`/nodes/${encodeURIComponent(nodeId)}/config`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),

  getSchedules: (nodeId: string) =>
    request<SchedulesResponse>(
      `/nodes/${encodeURIComponent(nodeId)}/schedules`,
    ),

  putSchedules: (nodeId: string, body: PutSchedulesBody) =>
    request<NodeConfig>(`/nodes/${encodeURIComponent(nodeId)}/schedules`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),

  dispatch: (nodeId: string, body: DispatchBody) =>
    request<DispatchResponse>(
      `/nodes/${encodeURIComponent(nodeId)}/dispatch`,
      {
        method: 'POST',
        body: JSON.stringify(body),
      },
    ),

  listResults: (query?: ListResultsQuery) => {
    const params = new URLSearchParams()
    if (query?.node_id) params.set('node_id', query.node_id)
    if (query?.limit != null) params.set('limit', String(query.limit))
    const qs = params.toString()
    return request<ResultRow[]>(`/results${qs ? `?${qs}` : ''}`)
  },

  listSkillGroups: () => request<SkillGroupsResponse>('/fleet/skill-groups'),

  pushSkillGroup: (body: PushSkillGroupBody) =>
    request<PushSkillGroupResponse>('/fleet/skill-groups/push', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  tokensStatus: () => request<TokensStatus>('/tokens'),

  createToken: () =>
    request<CreateTokenResponse>('/tokens', { method: 'POST' }),

  listSkills: () => request<SkillsListResponse>('/skills'),

  getSkill: (name: string) =>
    request<SkillDetail>(`/skills/${encodeURIComponent(name)}`),

  getSkillVersion: (name: string, version: string) =>
    request<SkillVersionDetail>(
      `/skills/${encodeURIComponent(name)}/versions/${encodeURIComponent(version)}`,
    ),

  uploadSkill: (bytes: ArrayBuffer) =>
    request<SkillPublishResponse>('/skills', {
      method: 'POST',
      body: bytes,
      headers: { 'Content-Type': 'application/vnd.novbot.skill' },
    }),

  installSkill: (name: string, body: InstallSkillBody) =>
    request<SkillChangeResponse>(`/skills/${encodeURIComponent(name)}/install`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  rollbackSkill: (name: string, body: RollbackSkillBody) =>
    request<SkillChangeResponse>(
      `/skills/${encodeURIComponent(name)}/rollback`,
      {
        method: 'POST',
        body: JSON.stringify(body),
      },
    ),

  uninstallSkill: (name: string, body: UninstallSkillBody) =>
    request<SkillChangeResponse>(
      `/skills/${encodeURIComponent(name)}/uninstall`,
      {
        method: 'POST',
        body: JSON.stringify(body),
      },
    ),

  getNodeSkills: (nodeId: string) =>
    request<NodeSkillsResponse>(
      `/nodes/${encodeURIComponent(nodeId)}/skills`,
    ),
}

/** Parse specs_json / schedules_json from NodeConfig for editors. */
export function parseJsonField(raw: string, fallback: unknown = []): unknown {
  try {
    return JSON.parse(raw) as unknown
  } catch {
    return fallback
  }
}

export function formatTime(iso: string | null | undefined): string {
  if (!iso) return '—'
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleString()
}

export function apiErrorCode(error: unknown): string | null {
  if (!(error instanceof ApiError)) return null
  const body = error.body
  if (!body || typeof body !== 'object' || !('code' in body)) return null
  const code = (body as { code?: unknown }).code
  return typeof code === 'string' && code.trim() ? code : null
}

export function isCapabilitiesChanged(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.status === 409 &&
    apiErrorCode(error) === 'capabilities_changed'
  )
}

function stringList(value: unknown): string[] {
  if (!Array.isArray(value)) return []
  return value.filter((item): item is string => typeof item === 'string')
}

/** `409 skill_in_use` carries the referencing spec and schedule ids. */
export function skillInUseRefs(
  error: unknown,
): { specIds: string[]; scheduleIds: string[] } | null {
  if (!(error instanceof ApiError) || apiErrorCode(error) !== 'skill_in_use') {
    return null
  }
  const body = error.body
  if (!body || typeof body !== 'object') {
    return { specIds: [], scheduleIds: [] }
  }
  const record = body as { spec_ids?: unknown; schedule_ids?: unknown }
  return {
    specIds: stringList(record.spec_ids),
    scheduleIds: stringList(record.schedule_ids),
  }
}
