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
  role: 'user' | 'assistant' | 'reasoning' | 'tool'
  id?: string
  turnId?: string
  text: string
  truncated: boolean
  tool?: {
    itemId: string
    category: 'shell' | 'file' | 'dynamic'
    lifecycle: 'unknown' | 'completed' | 'failed' | 'endedUnknown'
    name?: string
    command?: string
    cwd?: string
    target?: string
  }
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

/** Only server-provided text is projected; attachments and injected context stay out. */
function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown> : undefined
}

function identity(value: unknown): string | undefined {
  if (typeof value === 'number' && Number.isSafeInteger(value)) return String(value)
  return typeof value === 'string' && value.length > 0 && value.length <= 1024 ? value : undefined
}

function bounded(text: string): { text: string; truncated: boolean } {
  if (text.length <= HISTORY_TEXT_LIMIT) return { text, truncated: false }
  let end = HISTORY_TEXT_LIMIT
  // Do not cut an emoji's UTF-16 surrogate pair in half.
  if (/[\uD800-\uDBFF]/.test(text[end - 1]!)) end--
  return { text: text.slice(0, end), truncated: true }
}

function messageOf(role: HistoryMessage['role'], text: string, data: Record<string, unknown>, id?: string): HistoryMessage {
  const turnId = identity(data.turn)
  return { role, ...bounded(text), ...(id === undefined ? {} : { id }), ...(turnId === undefined ? {} : { turnId }) }
}

function messagesOf(event: SessionEvent): HistoryMessage[] {
  const data = record(event.data)
  if (data === undefined) return []
  let content: unknown
  let id: string | undefined
  if (event.type === 'user/message') {
    if (record(data.source)?.kind !== 'user') return []
    content = data.content
    id = identity(data.id)
  } else if (event.type === 'assistant/message') {
    const message = record(data.message)
    content = message?.content
    id = identity(message?.id)
  } else {
    return []
  }
  if (!Array.isArray(content)) return []
  const messages: HistoryMessage[] = []
  const textMessages = new Map<HistoryMessage['role'], HistoryMessage>()
  for (const raw of content) {
    const block = record(raw)
    if (block?.type === 'tool-call' && event.type === 'assistant/message') {
      const callId = identity(block.id)
      if (callId !== undefined) {
        const message = messageOf('tool', `▸ unknown · ${toolLabel(block)}`, data, callId)
        let input = block.arguments
        if (typeof input === 'string') {
          try { input = JSON.parse(input) } catch { input = undefined }
        }
        const fields = record(input)
        const command = fields?.command ?? fields?.cmd
        const target = fields?.file_path ?? fields?.path
        const cwd = fields?.workdir ?? fields?.cwd
        const short = (value: unknown): string | undefined => {
          if (typeof value !== 'string') return undefined
          let end = Math.min(512, value.length)
          if (end < value.length && /[\uD800-\uDBFF]/.test(value[end - 1]!)) end--
          return value.slice(0, end)
        }
        const commandText = short(Array.isArray(command) && command.every(part => typeof part === 'string') ? command.join(' ') : command)
        const nameText = short(block.name)
        const targetText = short(target)
        const cwdText = short(cwd)
        message.tool = {
          itemId: callId, lifecycle: 'unknown',
          category: command !== undefined ? 'shell' : target !== undefined ? 'file' : 'dynamic',
          ...(nameText === undefined ? {} : { name: nameText }),
          ...(commandText === undefined ? {} : { command: commandText }),
          ...(targetText === undefined ? {} : { target: targetText }),
          ...(cwdText === undefined ? {} : { cwd: cwdText }),
        }
        messages.push(message)
      }
      continue
    }
    if (typeof block?.text !== 'string' || block.text.trim() === '') continue
    const role = block.type === 'reasoning' && event.type === 'assistant/message' ? 'reasoning'
      : block.type === 'text' ? (event.type === 'user/message' ? 'user' : 'assistant') : undefined
    if (role === undefined) continue
    const previous = textMessages.get(role)
    if (previous === undefined) {
      const message = messageOf(role, block.text, data, id)
      messages.push(message)
      textMessages.set(role, message)
    } else if (!previous.truncated) {
      Object.assign(previous, bounded(`${previous.text}\n${block.text}`))
    }
  }
  return messages
}

function toolLabel(data: Record<string, unknown>): string {
  let input = data.arguments
  if (typeof input === 'string') {
    try { input = JSON.parse(input) } catch { input = undefined }
  }
  const fields = record(input)
  const command = fields?.command ?? fields?.cmd
  if (typeof command === 'string' && command.trim() !== '') return command.split(/\r?\n/, 1)[0]!
  if (Array.isArray(command) && command.every(part => typeof part === 'string')) return command.join(' ').split(/\r?\n/, 1)[0]!
  const name = typeof data.name === 'string' ? data.name : 'ACP tool call'
  const target = fields?.file_path ?? fields?.path ?? fields?.pattern ?? fields?.url ?? fields?.query
  return (typeof target === 'string' ? `${name} ${target}` : name).split(/\r?\n/, 1)[0]!
}

/**
 * Read the visible conversation, including server reasoning and tool summaries.
 * Tool results update only the matching turn/call; raw tool output is not retained.
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
  const tools = new Map<string, HistoryMessage>()
  let omitted = 0
  const append = (message: HistoryMessage): void => {
    messages.push(message)
    if (messages.length > HISTORY_MESSAGE_LIMIT) {
      const removed = messages.shift()!
      for (const [key, tool] of tools) if (tool === removed) tools.delete(key)
      omitted++
    }
  }
  const toolKey = (turn: unknown, id: string): string => JSON.stringify([identity(turn) ?? null, id])
  for (const event of surface.events) {
    for (const message of messagesOf(event)) {
      if (message.role === 'tool' && message.id !== undefined) {
        const key = toolKey(message.turnId, message.id)
        if (tools.has(key)) continue
        append(message)
        tools.set(key, message)
      } else {
        append(message)
      }
    }
    if (event.type !== 'tool/result') continue
    const data = record(event.data)
    const resultMessage = record(data?.message)
    // Saved pre-block sessions put call identity/outcome on the tool message itself.
    const results = identity(resultMessage?.toolCallId) !== undefined ? [resultMessage]
      : Array.isArray(resultMessage?.content) ? resultMessage.content : []
    if (data === undefined) continue
    for (const raw of results) {
      const result = record(raw)
      const id = identity(result?.toolCallId)
      if (id === undefined) continue
      const message = tools.get(toolKey(data.turn, id))
      if (message === undefined || message.tool?.lifecycle !== 'unknown') continue
      const status = record(data.error)?.code === 'TOOL_OUTCOME_UNKNOWN' ? 'ended, outcome unknown'
        : result?.isError === true ? 'failed'
        : result?.isError === false || result?.isError === undefined ? 'done' : 'unknown'
      message.tool.lifecycle = status === 'done' ? 'completed' : status === 'failed' ? 'failed' : status === 'ended, outcome unknown' ? 'endedUnknown' : 'unknown'
      const final = bounded(message.text.replace('▸ unknown', `▸ ${status}`))
      message.text = final.text
      message.truncated ||= final.truncated
    }
  }
  return { sessionId, messages, omitted }
}
