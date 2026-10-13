/**
 * Live facts that dsh's ACP projection does not carry, pushed to the TUI as
 * `zf/session/event` notifications.
 *
 * dsh's ACP server forwards committed messages only: a whole thinking or text
 * block arrives at once, and todo lists never arrive. The host still knows
 * when the model starts thinking or answering and what the agent's todo list
 * is, so the bridge relays those facts, together with model request retries
 * and context compaction. Payloads are bounded and carry no model output: a
 * stage is a label, a todo is the text the agent chose to show the user, and a
 * retry carries the host's failure code, never the provider's message.
 *
 * @module zf-tui-bridge/events
 */

import type { Frame, HostContext } from './host.js'

export const EVENT_METHOD = 'zf/session/event'

const MAX_TODOS = 100
const MAX_TODO_CHARS = 512

/** What the model is doing within one request, as far as the stream shows. */
export type Stage = 'waiting' | 'thinking' | 'response'

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' ? (value as Record<string, unknown>) : undefined
}

/** Session id of a host session object, or of the agent that owns one. */
function sessionId(owner: unknown): string | undefined {
  const session = record(record(owner)?.['session']) ?? record(owner)
  const id = record(session?.['header'])?.['id'] ?? session?.['id']
  return typeof id === 'string' && id !== '' ? id : undefined
}

/** The stage a stream frame puts its request in; `undefined` keeps the current one. */
export function stageOf(frame: unknown): Stage | undefined {
  const value = record(frame)
  if (value?.['type'] === 'start') return 'waiting'
  if (value?.['type'] !== 'chunk') return undefined
  switch (record(value['chunk'])?.['type']) {
    case 'reasoning-delta':
    case 'reasoning':
      return 'thinking'
    case 'text-delta':
    case 'text':
    case 'tool-call-delta':
    case 'tool-call':
      return 'response'
    default:
      return undefined
  }
}

/** A todo list as the TUI shows it; anything malformed is dropped rather than guessed. */
export function todosOf(data: unknown): { content: string; status: string }[] | undefined {
  const todos = record(data)?.['todos']
  if (!Array.isArray(todos)) return undefined
  const result: { content: string; status: string }[] = []
  for (const item of todos.slice(0, MAX_TODOS)) {
    const todo = record(item)
    const content = todo?.['content']
    const status = todo?.['status']
    if (typeof content !== 'string' || typeof status !== 'string') continue
    if (status !== 'pending' && status !== 'in_progress' && status !== 'completed') continue
    result.push({ content: content.slice(0, MAX_TODO_CHARS), status })
  }
  return result
}

function count(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : undefined
}

/** One retry of a model request; the code is a short identifier or absent. */
export function retryOf(data: unknown): Record<string, unknown> | undefined {
  const value = record(data)
  const retry = count(value?.['retry'])
  if (retry === undefined) return undefined
  const maxRetries = count(value?.['maxRetries'])
  const delayMs = count(value?.['delayMs'])
  const code = record(value?.['failure'])?.['code']
  return {
    type: 'retry',
    retry,
    ...(maxRetries === undefined ? {} : { maxRetries }),
    ...(delayMs === undefined ? {} : { delayMs }),
    ...(typeof code === 'string' && /^[A-Za-z0-9_.-]{1,64}$/.test(code) ? { code } : {}),
  }
}

/** Start or end of one context compaction; an end with an error did not compact. */
export function compactionOf(type: unknown, data: unknown): Record<string, unknown> | undefined {
  const value = record(data)
  const id = value?.['compactionId']
  if (typeof id !== 'string' || id === '' || id.length > 128) return undefined
  if (type === 'compaction/start') return { type: 'compaction', id, state: 'start' }
  if (type === 'compaction/end') return { type: 'compaction', id, state: value?.['error'] === undefined ? 'end' : 'failed' }
  return undefined
}

/**
 * Relay stage changes, todo lists, retries and compaction of every live session.
 * @param ctx - host context; its listeners end with the plugin.
 * @param send - writes one frame to the TUI.
 */
export function relayEvents(ctx: HostContext, send: (frame: Frame) => void): void {
  const notify = (session: string, event: Record<string, unknown>): void =>
    send({ jsonrpc: '2.0', method: EVENT_METHOD, params: { sessionId: session, event } })

  // One notification per change of stage, not per token.
  const stages = new Map<string, Stage>()
  ctx.on(
    'agent/assistant-stream',
    (payload: unknown) => {
      const session = sessionId(record(payload)?.['agent'])
      const stage = stageOf(record(payload)?.['frame'])
      if (session === undefined || stage === undefined || stages.get(session) === stage) return
      if (stages.size >= 256 && !stages.has(session)) stages.clear()
      stages.set(session, stage)
      notify(session, { type: 'stage', stage })
    },
    { global: true },
  )

  ctx.on('session/event', (owner: unknown, event: unknown) => {
    const session = sessionId(owner)
    const type = record(event)?.['type']
    if (session === undefined) return
    if (type === 'todo/write') {
      const todos = todosOf(record(event)?.['data'])
      if (todos !== undefined) notify(session, { type: 'todos', todos })
    } else if (type === 'llm/retry') {
      const retry = retryOf(record(event)?.['data'])
      if (retry !== undefined) notify(session, retry)
    } else if (type === 'compaction/start' || type === 'compaction/end') {
      const compaction = compactionOf(type, record(event)?.['data'])
      if (compaction !== undefined) notify(session, compaction)
    } else if (type === 'turn/end') {
      stages.delete(session)
    }
  })
}
