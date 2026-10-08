/**
 * Pure presentation helpers for the Skills Hub console.
 * Label filters are node label maps (`role=db`), never tags.
 */

export type LabelChip = {
  id: string
  key: string
  value: string
}

export type NodePresence = 'any' | 'seen' | 'never_seen'

export type NodePick = {
  node_id: string
  hostname: string
  labels: unknown
  last_seen_at: string | null
}

const RISK_ORDER: Record<string, number> = {
  high: 0,
  medium: 1,
  low: 2,
}

export function nodeLabelMap(raw: unknown): Record<string, string> {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return {}
  const out: Record<string, string> = {}
  for (const [key, value] of Object.entries(raw)) {
    if (typeof value === 'string') out[key] = value
  }
  return out
}

/** Same rule as center `labels_match`: every wanted pair must equal the node label. */
export function labelsMatch(
  nodeLabels: Record<string, string>,
  want: Record<string, string>,
): boolean {
  return Object.entries(want).every(([key, value]) => nodeLabels[key] === value)
}

export function labelChipsFromNodes(nodes: { labels: unknown }[]): LabelChip[] {
  const seen = new Set<string>()
  const chips: LabelChip[] = []
  for (const node of nodes) {
    const labels = nodeLabelMap(node.labels)
    for (const key of Object.keys(labels).sort()) {
      const value = labels[key]
      const id = `${key}=${value}`
      if (seen.has(id)) continue
      seen.add(id)
      chips.push({ id, key, value })
    }
  }
  chips.sort((a, b) => a.id.localeCompare(b.id))
  return chips
}

/** One value per key, because `selector.labels` is a map. */
export function toggleLabelChip(active: LabelChip[], chip: LabelChip): LabelChip[] {
  if (active.some((row) => row.id === chip.id)) {
    return active.filter((row) => row.id !== chip.id)
  }
  return [...active.filter((row) => row.key !== chip.key), chip]
}

export function selectorLabels(active: LabelChip[]): Record<string, string> {
  const labels: Record<string, string> = {}
  for (const chip of active) labels[chip.key] = chip.value
  return labels
}

export function installTarget(input: {
  handPicked: string[]
  labels: Record<string, string>
}): {
  node_ids: string[]
  selector?: { labels: Record<string, string> }
} {
  const node_ids = [...new Set(input.handPicked.filter((id) => id.trim()))].sort()
  if (Object.keys(input.labels).length === 0) return { node_ids }
  return { node_ids, selector: { labels: { ...input.labels } } }
}

export function matchesSearch(node: NodePick, query: string): boolean {
  const needle = query.trim().toLowerCase()
  if (!needle) return true
  const labels = nodeLabelMap(node.labels)
  const blob = [
    node.node_id,
    node.hostname,
    ...Object.entries(labels).map(([key, value]) => `${key}=${value}`),
  ]
    .join(' ')
    .toLowerCase()
  return blob.includes(needle)
}

export function matchesPresence(
  lastSeen: string | null,
  status: NodePresence,
): boolean {
  if (status === 'any') return true
  const seen = lastSeen != null && lastSeen !== ''
  return status === 'seen' ? seen : !seen
}

export function filterNodes(
  nodes: NodePick[],
  filter: { search: string; status: NodePresence; labels: Record<string, string> },
): NodePick[] {
  const want = filter.labels
  const useLabels = Object.keys(want).length > 0
  return nodes.filter((node) => {
    if (!matchesSearch(node, filter.search)) return false
    if (!matchesPresence(node.last_seen_at, filter.status)) return false
    if (useLabels && !labelsMatch(nodeLabelMap(node.labels), want)) return false
    return true
  })
}

export function isBuiltinSource(source: string): boolean {
  return source === 'builtin'
}

export function sourceLabel(source: string): string {
  if (source === 'builtin') return 'Built-in'
  if (source === 'hub') return 'Hub'
  return source
}

/** First `published` row. The center returns versions newest-semver first. */
export function defaultInstallVersion(
  versions: { version: string; status: string }[],
): string {
  return (
    versions.find((row) => row.status === 'published')?.version ??
    versions[0]?.version ??
    ''
  )
}

function hasGlob(scope: string): boolean {
  return scope.includes('*') || scope.includes('?')
}

/**
 * One operator-facing line per grant on the version payload.
 * `fs.read:/etc/os-release` is "Read file `/etc/os-release`".
 * Scope-less grants use the payload description ("Read system information").
 */
export function capabilityPromptLine(cap: {
  name: string
  scope: string | null
  description: string
  grant: string
}): string {
  const scope = cap.scope?.trim() ?? ''
  if (cap.name === 'fs.read' && scope) {
    if (hasGlob(scope)) return `Read files matching \`${scope}\``
    return `Read file \`${scope}\``
  }
  if (cap.name === 'fs.stat' && scope) {
    return `See metadata (owner, permissions, size) of \`${scope}\``
  }
  if (cap.name === 'fs.list' && scope) {
    return `List entries of directory \`${scope}\``
  }
  if (cap.name === 'env.read' && scope) {
    return `Read environment variable \`${scope}\``
  }
  const description = cap.description.trim().replace(/\.$/, '')
  if (description) return description
  return cap.grant
}

export function sortCapabilities<T extends { risk: string; grant: string }>(
  caps: T[],
): T[] {
  return [...caps].sort((a, b) => {
    const risk = (RISK_ORDER[a.risk] ?? 9) - (RISK_ORDER[b.risk] ?? 9)
    if (risk !== 0) return risk
    return a.grant.localeCompare(b.grant)
  })
}

export function outcomeHeadline(outcome: string): string {
  switch (outcome) {
    case 'pending':
      return 'Pending'
    case 'queued':
      return 'Queued'
    case 'already_installed':
      return 'Already installed'
    case 'node_too_old':
      return 'Node too old'
    case 'platform_unsupported':
      return 'Platform unsupported'
    case 'capability_unsupported':
      return 'Capability unsupported'
    case 'node_unknown':
      return 'Unknown node'
    case 'no_previous_version':
      return 'No previous version'
    default:
      return outcome
  }
}

/** `pending` and `queued` are the outcomes that bump desired generation. */
export function generationChanged(outcome: string): boolean {
  return outcome === 'pending' || outcome === 'queued'
}

export function outcomeDetail(outcome: string, generation: number): string {
  switch (outcome) {
    case 'pending':
      return `Desired generation is now ${generation}. The node is online and has not acknowledged it yet.`
    case 'queued':
      return `Desired generation is now ${generation}. The node was offline and receives the desired set when it reconnects.`
    case 'already_installed':
      return `This version was already desired. The desired generation was not changed (still ${generation}).`
    case 'node_unknown':
      return 'The center has no such node. No desired generation was written.'
    case 'node_too_old':
    case 'platform_unsupported':
    case 'capability_unsupported':
    case 'no_previous_version':
      return `The desired generation was not changed (still ${generation}).`
    default:
      return `Reported generation ${generation}.`
  }
}

export function resultSummary(perNode: { outcome: string }[]): string {
  const changed = perNode.filter((row) => generationChanged(row.outcome)).length
  const already = perNode.filter((row) => row.outcome === 'already_installed').length
  if (changed === 0 && already > 0) return 'No desired generation changed.'
  const parts: string[] = []
  if (changed > 0) {
    parts.push(
      `Desired generation changed for ${changed} node${changed === 1 ? '' : 's'}.`,
    )
  } else {
    parts.push('The center accepted the request.')
  }
  if (already > 0 && changed > 0) {
    parts.push(
      `${already} node${already === 1 ? '' : 's'} already had this version, so that desired generation stayed the same.`,
    )
  }
  return parts.join(' ')
}

export function outcomePillClass(outcome: string): string {
  switch (outcome) {
    case 'pending':
      return 'pill pill-pending'
    case 'queued':
      return 'pill pill-queued'
    case 'already_installed':
      return 'pill pill-already'
    case 'node_too_old':
    case 'platform_unsupported':
    case 'capability_unsupported':
    case 'node_unknown':
    case 'no_previous_version':
      return 'pill pill-err'
    default:
      return 'pill pill-neutral'
  }
}
