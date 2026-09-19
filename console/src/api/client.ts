/**
 * NovBot center HTTP API client.
 *
 * Browser traffic must stay same-origin HTTPS `/v1` (relative path in production).
 * In Vite dev, `vite.config.ts` proxies `/v1` (and `/health`) to the center.
 * Shapes match crates/novbot-center/src/http.rs + db.rs.
 */

const API_BASE = '/v1'

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

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const url = path.startsWith('http') ? path : `${API_BASE}${path}`
  const res = await fetch(url, {
    ...init,
    headers: {
      Accept: 'application/json',
      ...(init?.body ? { 'Content-Type': 'application/json' } : {}),
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

async function requestAbsolute<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: {
      Accept: 'application/json',
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

export const api = {
  health: () => requestAbsolute<HealthResponse>('/health'),

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
