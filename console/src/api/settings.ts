/**
 * Console connection settings (localStorage).
 * Empty baseUrl keeps relative `/v1` (Vite proxy / same-origin).
 */

const STORAGE_KEY = 'novbot.console.settings'

export type ConsoleSettings = {
  /** Absolute center origin (http/https), no trailing slash. Empty = relative `/v1`. */
  baseUrl: string
  /** Bearer token sent on subsequent API requests when non-empty. */
  token: string
}

export const DEFAULT_SETTINGS: ConsoleSettings = {
  baseUrl: '',
  token: '',
}

export function loadSettings(): ConsoleSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return { ...DEFAULT_SETTINGS }
    const parsed = JSON.parse(raw) as Partial<ConsoleSettings>
    return {
      baseUrl: typeof parsed.baseUrl === 'string' ? parsed.baseUrl : '',
      token: typeof parsed.token === 'string' ? parsed.token : '',
    }
  } catch {
    return { ...DEFAULT_SETTINGS }
  }
}

export function saveSettings(next: ConsoleSettings): void {
  const cleaned: ConsoleSettings = {
    baseUrl: next.baseUrl.trim().replace(/\/+$/, ''),
    token: next.token.trim(),
  }
  localStorage.setItem(STORAGE_KEY, JSON.stringify(cleaned))
}

/** Normalize user-entered API base URL (allow http/https origins). */
export function normalizeBaseUrl(input: string): string {
  const t = input.trim().replace(/\/+$/, '')
  if (!t) return ''
  if (t.endsWith('/v1')) return t.slice(0, -3).replace(/\/+$/, '')
  return t
}

export function settingsEventName(): string {
  return 'novbot-console-settings'
}

export function notifySettingsChanged(): void {
  window.dispatchEvent(new Event(settingsEventName()))
}
