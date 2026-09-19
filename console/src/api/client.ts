/**
 * NovBot center HTTP API client.
 *
 * Browser traffic must stay same-origin HTTPS `/v1` (relative path in production).
 * In Vite dev, `vite.config.ts` proxies `/v1` to the center (default http://127.0.0.1:8080).
 */

const API_BASE = '/v1'

export class ApiError extends Error {
  readonly status: number
  readonly body: unknown

  constructor(status: number, body: unknown, message?: string) {
    super(message ?? `API ${status}`)
    this.name = 'ApiError'
    this.status = status
    this.body = body
  }
}

async function request<T>(
  path: string,
  init?: RequestInit,
): Promise<T> {
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

/** Stub types — align with center HTTP later. */
export type NodeSummary = {
  node_id: string
  hostname?: string
  version?: string
  last_seen_at?: string
  labels?: Record<string, string>
}

export type NodeConfig = {
  node_id: string
  config_generation?: number
  specs?: unknown[]
  schedules?: unknown[]
}

export type ResultRow = {
  id?: number
  node_id: string
  run_id?: string
  spec_id?: string
  status?: string
  observed_at?: string
  received_at?: string
  payload?: unknown
}

export const api = {
  listNodes: () => request<NodeSummary[]>('/nodes'),
  getConfig: (nodeId: string) =>
    request<NodeConfig>(`/nodes/${encodeURIComponent(nodeId)}/config`),
  putConfig: (nodeId: string, body: unknown) =>
    request<unknown>(`/nodes/${encodeURIComponent(nodeId)}/config`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  getSchedules: (nodeId: string) =>
    request<unknown>(`/nodes/${encodeURIComponent(nodeId)}/schedules`),
  putSchedules: (nodeId: string, body: unknown) =>
    request<unknown>(`/nodes/${encodeURIComponent(nodeId)}/schedules`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  dispatch: (nodeId: string, body: unknown) =>
    request<unknown>(`/nodes/${encodeURIComponent(nodeId)}/dispatch`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  listResults: (query?: { node_id?: string; limit?: number }) => {
    const params = new URLSearchParams()
    if (query?.node_id) params.set('node_id', query.node_id)
    if (query?.limit != null) params.set('limit', String(query.limit))
    const qs = params.toString()
    return request<ResultRow[]>(`/results${qs ? `?${qs}` : ''}`)
  },
}
