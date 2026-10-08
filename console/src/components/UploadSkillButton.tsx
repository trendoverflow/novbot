import { useRef, useState } from 'react'
import { api, apiErrorCode, type SkillPublishResponse } from '../api/client'
import { Button } from './Ui'

export function UploadSkillButton({
  disabled = false,
  onUploaded,
}: {
  disabled?: boolean
  onUploaded: (body: SkillPublishResponse) => void
}) {
  const inputRef = useRef<HTMLInputElement>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function onFile(file: File) {
    setBusy(true)
    setError(null)
    try {
      const body = await api.uploadSkill(await file.arrayBuffer())
      onUploaded(body)
    } catch (err) {
      const code = apiErrorCode(err)
      const message = err instanceof Error ? err.message : String(err)
      setError(code ? `${message} (${code})` : message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <span className="upload-slot">
      <input
        ref={inputRef}
        type="file"
        accept=".nbskill,application/vnd.novbot.skill"
        hidden
        disabled={disabled || busy}
        onChange={(event) => {
          const file = event.target.files?.[0]
          event.target.value = ''
          if (file) void onFile(file)
        }}
      />
      <Button
        variant="ghost"
        disabled={disabled || busy}
        title={disabled ? 'Built-in, ships with the node' : undefined}
        onClick={() => inputRef.current?.click()}
      >
        {busy ? 'Uploading…' : 'Upload package'}
      </Button>
      {error ? (
        <span className="upload-error" role="alert">
          {error}
        </span>
      ) : null}
    </span>
  )
}
