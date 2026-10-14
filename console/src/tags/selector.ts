/** Kubernetes-style tag selectors used by the nodes filter bar. */

export type FilterOp = 'eq' | 'neq' | 'exists' | 'not' | 'in' | 'notin'

export type FilterChip = {
  key: string
  op: FilterOp
  values: string[]
}

export const RECOMMENDED_KEYS = ['env', 'role', 'site', 'region', 'owner', 'team']

export const VALUE_HINTS: Record<string, string[]> = {
  env: ['dev', 'test', 'staging', 'prod'],
  role: ['app', 'db', 'web', 'cache', 'lb', 'mq', 'bastion'],
}

const KEY_RE = /^[a-z][a-z0-9._-]{0,62}$/
const VALUE_RE = /^[a-z0-9._-]{1,63}$/

export function isReservedKey(key: string): boolean {
  return key === 'sys' || key.startsWith('sys.')
}

export function tagKeyError(key: string): string | null {
  if (!KEY_RE.test(key)) return 'Tag key must be lowercase and match a-z, digits, ".", "_" or "-".'
  if (isReservedKey(key)) return 'Keys sys and sys.* are reserved for system tags.'
  return null
}

export function tagValueError(value: string): string | null {
  if (!VALUE_RE.test(value)) {
    return 'Tag value must be lowercase and match a-z, digits, ".", "_" or "-".'
  }
  return null
}

/** Lowercase suggestion. Null when the text is already lowercase or lowercase is still invalid. */
export function lowercaseSuggestion(
  key: string,
  value: string,
): { key: string; value: string } | null {
  const nextKey = key.toLowerCase()
  const nextValue = value.toLowerCase()
  if (nextKey === key && nextValue === value) return null
  if (tagKeyError(nextKey) || tagValueError(nextValue)) return null
  return { key: nextKey, value: nextValue }
}

export function chipLabel(chip: FilterChip): string {
  const values = uniqueSorted(chip.values)
  switch (chip.op) {
    case 'eq':
      return `${chip.key}=${values[0] ?? ''}`
    case 'neq':
      return `${chip.key}!=${values[0] ?? ''}`
    case 'exists':
      return chip.key
    case 'not':
      return `!${chip.key}`
    case 'in':
      return `${chip.key} in (${values.join(',')})`
    case 'notin':
      return `${chip.key} notin (${values.join(',')})`
  }
}

export function canonicalSelector(chips: FilterChip[]): string {
  return chips
    .map(normalizeChip)
    .filter((chip): chip is FilterChip => chip !== null)
    .sort((a, b) => a.key.localeCompare(b.key) || a.op.localeCompare(b.op))
    .map(chipLabel)
    .join(',')
}

/** Two equality chips on one key become a single `in` chip. */
export function addChip(chips: FilterChip[], next: FilterChip): FilterChip[] {
  const chip = normalizeChip(next)
  if (!chip) return chips
  if (chip.op === 'eq' || chip.op === 'in') {
    const index = chips.findIndex(
      (item) => item.key === chip.key && (item.op === 'eq' || item.op === 'in'),
    )
    if (index >= 0) {
      const prev = chips[index]
      const values = uniqueSorted([...prev.values, ...chip.values])
      const merged: FilterChip = {
        key: chip.key,
        op: values.length > 1 ? 'in' : 'eq',
        values,
      }
      return chips.map((item, i) => (i === index ? merged : item))
    }
  }
  return [...chips, chip]
}

export function removeChip(chips: FilterChip[], index: number): FilterChip[] {
  return chips.filter((_, i) => i !== index)
}

/**
 * Parse a selector into chips.
 * Returns [] for blank and `*`. Returns null when the text is not a selector we can edit.
 */
export function parseSelector(raw: string): FilterChip[] | null {
  const text = raw.trim()
  if (!text || text === '*') return []
  const parts = splitRequirements(text)
  let chips: FilterChip[] = []
  for (const part of parts) {
    const chip = parseRequirement(part.trim())
    if (!chip) return null
    chips = addChip(chips, chip)
  }
  return chips
}

function normalizeChip(chip: FilterChip): FilterChip | null {
  const key = chip.key.trim()
  if (!KEY_RE.test(key)) return null
  const values = uniqueSorted(chip.values.map((value) => value.trim()).filter(Boolean))
  if ((chip.op === 'eq' || chip.op === 'neq') && (values.length !== 1 || !VALUE_RE.test(values[0]))) {
    return null
  }
  if ((chip.op === 'in' || chip.op === 'notin') && (values.length === 0 || values.some((value) => !VALUE_RE.test(value)))) {
    return null
  }
  if ((chip.op === 'exists' || chip.op === 'not') && values.length !== 0) return null
  return { key, op: chip.op, values: chip.op === 'exists' || chip.op === 'not' ? [] : values }
}

function parseRequirement(part: string): FilterChip | null {
  const set = /^([a-z][a-z0-9._-]{0,62})\s+(in|notin)\s+\(([^)]*)\)$/.exec(part)
  if (set) {
    const values = set[3]
      .split(',')
      .map((value) => value.trim())
      .filter(Boolean)
    return normalizeChip({
      key: set[1],
      op: set[2] === 'notin' ? 'notin' : 'in',
      values,
    })
  }
  const notEq = /^([a-z][a-z0-9._-]{0,62})!=([a-z0-9._-]{1,63})$/.exec(part)
  if (notEq) return { key: notEq[1], op: 'neq', values: [notEq[2]] }
  const eq = /^([a-z][a-z0-9._-]{0,62})=([a-z0-9._-]{1,63})$/.exec(part)
  if (eq) return { key: eq[1], op: 'eq', values: [eq[2]] }
  const missing = /^!([a-z][a-z0-9._-]{0,62})$/.exec(part)
  if (missing) return { key: missing[1], op: 'not', values: [] }
  const exists = /^([a-z][a-z0-9._-]{0,62})$/.exec(part)
  if (exists) return { key: exists[1], op: 'exists', values: [] }
  return null
}

function splitRequirements(text: string): string[] {
  const parts: string[] = []
  let depth = 0
  let buf = ''
  for (const ch of text) {
    if (ch === '(') depth += 1
    if (ch === ')' && depth > 0) depth -= 1
    if (ch === ',' && depth === 0) {
      parts.push(buf)
      buf = ''
      continue
    }
    buf += ch
  }
  if (buf.trim()) parts.push(buf)
  return parts
}

function uniqueSorted(values: string[]): string[] {
  return [...new Set(values)].sort((a, b) => a.localeCompare(b))
}
