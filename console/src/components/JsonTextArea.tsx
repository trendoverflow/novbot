type Props = {
  id?: string
  label: string
  value: string
  onChange: (next: string) => void
  rows?: number
  disabled?: boolean
}

export function JsonTextArea({
  id,
  label,
  value,
  onChange,
  rows = 16,
  disabled,
}: Props) {
  return (
    <label className="field">
      <span className="field-label">{label}</span>
      <textarea
        id={id}
        className="json-editor"
        spellCheck={false}
        rows={rows}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      />
    </label>
  )
}
