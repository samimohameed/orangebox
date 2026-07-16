import { useCallback, useEffect, useRef, useState } from 'react'
import './App.css'
import { fetchSearch, fetchStatus, fetchTimeline, fetchTranscript } from './api'
import Sidebar, { type ToolFilter } from './components/Sidebar'
import Transcript from './components/Transcript'
import type { SearchHit, SessionSummary, Status, Transcript as TranscriptData } from './types'

export default function App() {
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [hits, setHits] = useState<SearchHit[] | null>(null)
  const [query, setQuery] = useState('')
  const [toolFilter, setToolFilter] = useState<ToolFilter>('all')
  const [transcript, setTranscript] = useState<TranscriptData | null>(null)
  const [status, setStatus] = useState<Status | null>(null)
  const [toast, setToast] = useState('')

  const toastTimer = useRef<number | undefined>(undefined)
  const showToast = useCallback((text: string) => {
    setToast(text)
    window.clearTimeout(toastTimer.current)
    toastTimer.current = window.setTimeout(() => setToast(''), 1800)
  }, [])

  const refreshTimeline = useCallback(() => {
    fetchTimeline().then(setSessions).catch(() => {})
    fetchStatus().then(setStatus).catch(() => {})
  }, [])

  useEffect(() => {
    refreshTimeline()
    const interval = window.setInterval(refreshTimeline, 15000)
    return () => window.clearInterval(interval)
  }, [refreshTimeline])

  // Debounced search.
  useEffect(() => {
    const q = query.trim()
    if (!q) {
      setHits(null)
      return
    }
    const timer = window.setTimeout(() => {
      fetchSearch(q)
        .then(setHits)
        .catch((e) => showToast(e.message))
    }, 250)
    return () => window.clearTimeout(timer)
  }, [query, showToast])

  const openSession = useCallback(
    (id: string) => {
      fetchTranscript(id)
        .then(setTranscript)
        .catch((e) => showToast(e.message))
    },
    [showToast],
  )

  return (
    <div className="app">
      <Sidebar
        query={query}
        onQueryChange={setQuery}
        toolFilter={toolFilter}
        onToolFilterChange={setToolFilter}
        sessions={sessions}
        hits={hits}
        activeId={transcript?.session.id ?? null}
        onSelect={openSession}
        status={status}
      />
      <main className="main">
        {transcript ? (
          <Transcript data={transcript} onToast={showToast} />
        ) : (
          <div className="empty">
            <div className="big">📼</div>
            <div>
              <strong>Pick a session</strong>
            </div>
            <div>
              Everything your AI tools said is recorded here —
              <br />
              even if the tool itself lost it.
            </div>
          </div>
        )}
      </main>
      <div className={`toast ${toast ? 'show' : ''}`}>{toast}</div>
    </div>
  )
}
