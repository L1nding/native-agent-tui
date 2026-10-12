/**
 * Stored sessions as the TUI's sessions page needs them: a titled list for one
 * workspace and the readable messages of one session. Both are read from dsh's
 * own services; nothing is activated, written or cached here.
 *
 * @module zf-tui-bridge/sessions
 */

import type { HostContext, SessionEvent, SessionHeader, SessionQuery } from './host.js'

/** Most messages returned for one session; older ones are counted, not sent. */
export const HISTORY_MESSAGE_LIMIT = 200
/** Longest text of one returned message, in UTF-16 code units. */
export const HISTORY_TEXT_LIMIT = 16_384

export interface SessionRow {
  id: string
  cwd?: string
  createdAt?: number
  /** Latest of creation and the newest title event; absent facts are not invented. */
  updatedAt?: number
  title?: string
  /** Loaded in this dsh process right now. */
  live: boolean
}

export interface HistoryMessage {
  role: 'user' | 'assistant'
  text: string
  truncated: boolean
}

/** Windows paths differ in separators and letter case for the same directory. */
function sameDirectory(left: string, right: string): boolean {
  const normalize = (path: string): string => {
    const slashed = path.replaceAll('\\', '/').replace(/\/+$/, '')
    return process.platform === 'win32' ? slashed.toLowerCase() : slashed
  }
  return normalize(left) === normalize(right)
}

function isChild(header: SessionHeader): boolean {
  return header.origin === 'subagent' || header.parentSession !== undefined || (header.delegationDepth ?? 0) > 0
}

/**
 * List root sessions, newest first.
 * @param ctx - host context.
 * @param cwd - when given, only sessions of this workspace.
 */
export async function listSessions(ctx: HostContext, cwd: string | undefined): Promise<{
  sessions: SessionRow[]
  titles: 'available' | 'unavailable'
}> {
  const query = ctx.get('sessionQuery')
  const headers: { header: SessionHeader; live: boolean }[] =
    query === undefined
      ? (await ctx.sessionPersistence.list()).map(({ header }) => ({ header, live: false }))
      : (await query.listSessions()).map(({ header, live }) => ({ header, live }))
  const visible = headers.filter(
    ({ header }) => !isChild(header) && (cwd === undefined || (header.cwd !== undefined && sameDirectory(header.cwd, cwd))),
  )
  const titles = new Map<string, { title: string; updatedAt: number }>()
  if (query !== undefined && visible.length > 0) {
    for (const result of await query.readTitleSnapshots(visible.map(({ header }) => header.id))) {
      // One unreadable session leaves its own row untitled; the list stays whole.
      if (result.status !== 'fulfilled' || result.value.title === undefined) continue
      titles.set(result.sessionId, { title: result.value.title.title, updatedAt: result.value.title.updatedAt })
    }
  }
  const sessions = visible.map(({ header, live }): SessionRow => {
    const title = titles.get(header.id)
    const updatedAt = Math.max(header.createdAt ?? 0, title?.updatedAt ?? 0)
    return {
      id: header.id,
      live,
      ...(header.cwd === undefined ? {} : { cwd: header.cwd }),
      ...(header.createdAt === undefined ? {} : { createdAt: header.createdAt }),
      ...(updatedAt === 0 ? {} : { updatedAt }),
      ...(title === undefined ? {} : { title: title.title }),
    }
  })
  sessions.sort((left, right) => (right.updatedAt ?? 0) - (left.updatedAt ?? 0))
  return { sessions, titles: query === undefined ? 'unavailable' : 'available' }
}

/** Text blocks only: reasoning, tool calls and attachments are not conversation text. */
function textOf(content: unknown): string {
  if (!Array.isArray(content)) return ''
  const parts: string[] = []
  for (const block of content) {
    if (block === null || typeof block !== 'object') continue
    const { type, text } = block as { type?: unknown; text?: unknown }
    if (type === 'text' && typeof text === 'string' && text.trim() !== '') parts.push(text)
  }
  return parts.join('\n')
}

function messageOf(event: SessionEvent): HistoryMessage | undefined {
  const data = event.data
  if (data === null || typeof data !== 'object') return undefined
  let role: HistoryMessage['role']
  let text: string
  if (event.type === 'user/message') {
    // dsh also stores injected context as user messages; only `source.kind: user` was typed by a person.
    const source = (data as { source?: unknown }).source
    if (source === null || typeof source !== 'object' || (source as { kind?: unknown }).kind !== 'user') return undefined
    role = 'user'
    text = textOf((data as { content?: unknown }).content)
  } else if (event.type === 'assistant/message') {
    role = 'assistant'
    const message = (data as { message?: unknown }).message
    text = message !== null && typeof message === 'object' ? textOf((message as { content?: unknown }).content) : ''
  } else {
    return undefined
  }
  if (text === '') return undefined
  const truncated = text.length > HISTORY_TEXT_LIMIT
  return { role, text: truncated ? text.slice(0, HISTORY_TEXT_LIMIT) : text, truncated }
}

/**
 * Read the user and assistant text of one session's current surface.
 * @param query - dsh's session query service.
 * @param sessionId - session to read.
 */
export async function readHistory(query: SessionQuery, sessionId: string): Promise<{
  sessionId: string
  messages: HistoryMessage[]
  /** Older messages left out by the limit. */
  omitted: number
}> {
  const surface = await query.readSurface(sessionId)
  const messages: HistoryMessage[] = []
  for (const event of surface.events) {
    const message = messageOf(event)
    if (message !== undefined) messages.push(message)
  }
  const omitted = Math.max(0, messages.length - HISTORY_MESSAGE_LIMIT)
  return { sessionId, messages: messages.slice(omitted), omitted }
}
