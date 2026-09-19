import type { ButtonHTMLAttributes, ReactNode } from 'react'

export function Loading({ label = 'Loading…' }: { label?: string }) {
  return <p className="muted">{label}</p>
}

export function ErrorBanner({ error }: { error: Error }) {
  return (
    <div className="alert alert-error" role="alert">
      <strong>Error</strong>
      <div>{error.message}</div>
    </div>
  )
}

export function SuccessBanner({ children }: { children: ReactNode }) {
  return (
    <div className="alert alert-ok" role="status">
      {children}
    </div>
  )
}

export function EmptyState({ children }: { children: ReactNode }) {
  return <p className="muted">{children}</p>
}

export function Toolbar({ children }: { children: ReactNode }) {
  return <div className="toolbar">{children}</div>
}

export function Button({
  variant = 'primary',
  className = '',
  type = 'button',
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: 'primary' | 'ghost'
}) {
  return (
    <button
      type={type}
      className={`btn btn-${variant} ${className}`.trim()}
      {...rest}
    />
  )
}
