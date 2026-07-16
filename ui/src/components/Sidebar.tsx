import type { SearchHit, SessionSummary, Status } from '../types'

const TOOLS = ['all', 'claude-code', 'antigravity'] as const
export type ToolFilter = (typeof TOOLS)[number]

export function fmtDate(ms: number): string {
  if (!ms) return ''
  return new Date(ms).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' })
}

export function projectName(project: string | null): string {
  if (!project) return 'unknown project'
  return project.split('/').filter(Boolean).pop() ?? project
}

/** Render a search snippet, turning the FTS `>>match<<` markers into <mark>. */
function Snippet({ text }: { text: string }) {
  const parts = text.split(/>>(.*?)<</g)
  return (
    <div className="title">
      {parts.map((part, i) => (i % 2 === 1 ? <mark key={i}>{part}</mark> : part))}
    </div>
  )
}

interface SidebarProps {
  query: string
  onQueryChange: (q: string) => void
  toolFilter: ToolFilter
  onToolFilterChange: (t: ToolFilter) => void
  sessions: SessionSummary[]
  hits: SearchHit[] | null
  activeId: string | null
  onSelect: (id: string) => void
  status: Status | null
}

export default function Sidebar(props: SidebarProps) {
  const searching = props.hits !== null

  const items = searching
    ? props.hits!.filter(
        (h) => props.toolFilter === 'all' || h.session.tool === props.toolFilter,
      )
    : props.sessions.filter(
        (s) => props.toolFilter === 'all' || s.tool === props.toolFilter,
      )

  return (
    <aside className="sidebar">
      <div className="brand">
        <span className="dot" />
        <h1>Orangebox</h1>
        <small>flight recorder</small>
      </div>

      <div className="searchbox">
        <input
          type="search"
          placeholder="Search every conversation…"
          value={props.query}
          onChange={(e) => props.onQueryChange(e.target.value)}
        />
      </div>

      <div className="chips">
        {TOOLS.map((tool) => (
          <button
            key={tool}
            className={`chip ${props.toolFilter === tool ? 'active' : ''}`}
            onClick={() => props.onToolFilterChange(tool)}
          >
            {tool}
          </button>
        ))}
      </div>

      <div className="list">
        {items.length === 0 && (
          <div className="item" style={{ color: 'var(--muted)', cursor: 'default' }}>
            {searching ? 'No matches.' : 'Archive is empty — run orangebox scan.'}
          </div>
        )}
        {searching
          ? props.hits!.map((hit, i) =>
              props.toolFilter !== 'all' && hit.session.tool !== props.toolFilter ? null : (
                <div
                  key={`${hit.session.id}-${i}`}
                  className={`item ${props.activeId === hit.session.id ? 'active' : ''}`}
                  onClick={() => props.onSelect(hit.session.id)}
                >
                  <div className="top">
                    <span className="tool">{hit.session.tool}</span>
                    <span>
                      {projectName(hit.session.project)} · {fmtDate(hit.created_at_ms)}
                    </span>
                  </div>
                  <Snippet text={hit.snippet} />
                </div>
              ),
            )
          : props.sessions.map((s) =>
              props.toolFilter !== 'all' && s.tool !== props.toolFilter ? null : (
                <div
                  key={s.id}
                  className={`item ${props.activeId === s.id ? 'active' : ''}`}
                  onClick={() => props.onSelect(s.id)}
                >
                  <div className="top">
                    <span className="tool">{s.tool}</span>
                    <span>
                      {projectName(s.project)} · {fmtDate(s.last_activity_ms)}
                    </span>
                  </div>
                  <div className="title">{s.title ?? '(untitled session)'}</div>
                </div>
              ),
            )}
      </div>

      <footer>
        {props.status ? (
          <>
            <span className="recdot" />
            {props.status.messages.toLocaleString()} messages · {props.status.sessions}{' '}
            sessions · recording
          </>
        ) : (
          'connecting…'
        )}
      </footer>
    </aside>
  )
}
