import { fetchExportText, exportUrl } from '../api'
import type { Transcript as TranscriptData } from '../types'
import { fmtDate, projectName } from './Sidebar'

/** Heuristic: render obviously code-shaped content in monospace. */
function looksLikeCode(content: string): boolean {
  const head = content.slice(0, 300)
  return (
    /^\s*[{[]/.test(head) ||
    /^(import|export|function|class|def|use|const|let|var|#include|package)\b/m.test(head)
  )
}

function roleLabel(role: string): string {
  return role === 'user' ? 'You' : role
}

interface TranscriptProps {
  data: TranscriptData
  onToast: (text: string) => void
}

export default function Transcript({ data, onToast }: TranscriptProps) {
  const s = data.session
  const nativeId = s.id.replace(/^[a-z-]+:/, '')

  const copyRecovery = async () => {
    const text = await fetchExportText(s.id)
    await navigator.clipboard.writeText(text)
    onToast('Recovery prompt copied — paste it into a new session')
  }

  return (
    <>
      <header className="session-header">
        <div className="info">
          <h2>{projectName(s.project)}</h2>
          <div className="meta">
            {s.tool} · {fmtDate(s.started_at_ms)} → {fmtDate(s.last_activity_ms)} ·{' '}
            {data.messages.length} message(s) ·{' '}
            <span className="mono">{nativeId.slice(0, 8)}…</span>
          </div>
        </div>
        <div className="actions">
          <button className="primary" onClick={copyRecovery}>
            Copy recovery prompt
          </button>
          <a href={exportUrl(s.id)} download={`recovered-${nativeId.slice(0, 8)}.md`}>
            <button>Download .md</button>
          </a>
        </div>
      </header>

      <div className="recovery-hint">
        {s.tool === 'claude-code' ? (
          <>
            Native resume available — run <code>claude --resume {nativeId}</code> in{' '}
            <code>{s.project ?? '~'}</code>, or use the recovery prompt.
          </>
        ) : s.tool === 'antigravity' ? (
          <>
            Recovered from Antigravity's local trajectory database. To continue the
            work: copy the recovery prompt and paste it into a new agent session.
          </>
        ) : (
          <>Copy the recovery prompt and paste it into a new session to restore this context.</>
        )}
      </div>

      <div className="transcript">
        {data.messages.map((m, i) => (
          <div key={i} className={`msg ${m.role}`}>
            <div className="who">
              {roleLabel(m.role)}
              {m.created_at_ms > 0 && <> · {fmtDate(m.created_at_ms)}</>}
            </div>
            <div className={`body ${looksLikeCode(m.content) ? 'code' : ''}`}>
              {m.content}
            </div>
          </div>
        ))}
      </div>
    </>
  )
}
