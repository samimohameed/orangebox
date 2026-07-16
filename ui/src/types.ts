export interface Session {
  id: string
  tool: string
  project: string | null
  started_at_ms: number
  last_activity_ms: number
}

export interface SessionSummary extends Session {
  message_count: number
  title: string | null
}

export interface SearchHit {
  session: Session
  snippet: string
  role: string
  created_at_ms: number
}

export interface Message {
  role: string
  content: string
  created_at_ms: number
}

export interface Transcript {
  session: Session
  messages: Message[]
}

export interface Status {
  sessions: number
  messages: number
  tools: [string, number][]
}
