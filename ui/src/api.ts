import type { SearchHit, SessionSummary, Status, Transcript } from './types'

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path)
  if (!res.ok) {
    const body = await res.json().catch(() => null)
    throw new Error(body?.error ?? res.statusText)
  }
  return res.json()
}

export const fetchTimeline = () => get<SessionSummary[]>('/api/timeline?n=300')
export const fetchSearch = (q: string) =>
  get<SearchHit[]>(`/api/search?q=${encodeURIComponent(q)}`)
export const fetchTranscript = (id: string) =>
  get<Transcript>(`/api/session?id=${encodeURIComponent(id)}`)
export const fetchStatus = () => get<Status>('/api/status')

export const exportUrl = (id: string) => `/api/export?id=${encodeURIComponent(id)}`

export async function fetchExportText(id: string): Promise<string> {
  const res = await fetch(exportUrl(id))
  if (!res.ok) throw new Error('export failed')
  return res.text()
}
