/**
 * Questions the agent puts to the user, such as a plan review.
 *
 * dsh pauses a tool call until someone answers, and asks the answerers
 * composed into the host. Its ACP server composes none, so `exit_plan_mode`
 * and `ask_user_question` fail there. The bridge answers by forwarding the
 * question to the TUI as a `zf/session/ask` request and waiting for its reply.
 * The TUI shows it in its existing request panel; nothing is answered here.
 *
 * @module zf-tui-bridge/questions
 */

import type { Frame, FrameId, HostContext } from './host.js'

export const ASK_METHOD = 'zf/session/ask'

const MAX_QUESTIONS = 64
const MAX_TEXT_CHARS = 6000
const MAX_OPTIONS = 12

interface Pending {
  /** Option labels of each question, to tell a choice from typed text. */
  labels: Map<string, Set<string>>
  previewHashes: Map<string, unknown>
  resolve(answer: unknown): void
  reject(error: Error): void
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' ? (value as Record<string, unknown>) : undefined
}

function text(value: unknown, limit = MAX_TEXT_CHARS): string {
  return typeof value === 'string' ? value.slice(0, limit) : ''
}

/** Who dismissed a question; the host reads the code, e.g. to keep planning. */
export type Dismissal = (message: string) => Error

/**
 * Answer the host's questions through the TUI.
 * @param ctx - host context; the answerer ends with the plugin.
 * @param send - writes one frame to the TUI.
 * @param dismissal - builds the error for a question the user did not answer.
 * @returns `settle`, which consumes the TUI's reply to a question and reports
 *   whether the frame was one.
 */
export function answerQuestions(
  ctx: HostContext,
  send: (frame: Frame) => void,
  dismissal: Dismissal = (message) => new Error(message),
): { settle(frame: Frame): boolean } {
  const pending = new Map<FrameId, Pending>()
  let next = 0

  ctx.on('user-questions/request', (request: unknown, pass: unknown) => {
    const value = record(request)
    const agent = record(value?.['agent'])
    const session = record(record(agent?.['session'])?.['header'])?.['id']
    const asked = Array.isArray(value?.['questions']) ? (value['questions'] as unknown[]) : []
    // Questions without an owning session are not this client's to show.
    if (typeof session !== 'string' || asked.length === 0 || asked.length > MAX_QUESTIONS) {
      return typeof pass === 'function' ? (pass as () => unknown)() : undefined
    }
    const labels = new Map<string, Set<string>>()
    const previewHashes = new Map<string, unknown>()
    const questions = asked.map((item) => {
      const question = record(item) ?? {}
      const options = (Array.isArray(question['options']) ? (question['options'] as unknown[]) : [])
        .slice(0, MAX_OPTIONS)
        .map((option) => ({ label: text(record(option)?.['label'], 200), description: text(record(option)?.['description'], 400) }))
        .filter((option) => option.label !== '')
      // The reply key must remain the host's exact item identity.
      const id = typeof question['id'] === 'string' ? question['id'] : ''
      labels.set(id, new Set(options.map((option) => option.label)))
      const preview = record(question['preview'])
      if (preview !== undefined) previewHashes.set(id, preview['data_sha256'])
      const detail = text(question['detail'])
      return {
        id,
        header: text(question['header'], 200),
        // The reviewed text, e.g. the plan, is part of what the user is asked.
        question: detail === '' ? text(question['question']) : `${text(question['question'], 400)}\n\n${detail}`,
        ...(question['preview'] === undefined ? {} : { preview: question['preview'] }),
        ...(options.length === 0 ? {} : { options, multiSelect: question['multiSelect'] === true }),
      }
    })
    const id = `zf-ask-${++next}`
    return new Promise((resolve, reject) => {
      const signal = value?.['signal'] as AbortSignal | undefined
      const withdraw = (): void => {
        if (!pending.delete(id)) return
        send({ jsonrpc: '2.0', method: 'zf/session/event', params: { sessionId: session, event: { type: 'ask', id, state: 'withdrawn' } } })
        reject(signal?.reason instanceof Error ? signal.reason : new Error('the question was withdrawn'))
      }
      pending.set(id, {
        labels,
        previewHashes,
        resolve: (answer) => {
          signal?.removeEventListener('abort', withdraw)
          resolve(answer)
        },
        reject: (error) => {
          signal?.removeEventListener('abort', withdraw)
          reject(error)
        },
      })
      if (signal?.aborted === true) return withdraw()
      signal?.addEventListener('abort', withdraw, { once: true })
      send({ jsonrpc: '2.0', id, method: ASK_METHOD, params: { sessionId: session, questions } })
    })
  })

  return {
    settle(frame) {
      const entry = frame.id === undefined ? undefined : pending.get(frame.id)
      if (entry === undefined || frame.method !== undefined) return false
      pending.delete(frame.id as FrameId)
      const given = record(record(frame.result)?.['answers'])
      if (frame.error !== undefined || given === undefined) {
        entry.reject(dismissal('the user dismissed the question'))
        return true
      }
      for (const [id, hash] of entry.previewHashes) {
        if (typeof hash !== 'string' || hash === '' || record(given[id])?.['preview_data_sha256'] !== hash) {
          entry.reject(new Error(`missing or mismatched preview_data_sha256 for question ${id}`))
          return true
        }
      }
      const answers = [...entry.labels].map(([id, labels]) => {
        const answer = record(given[id])
        const values = answer?.['answers']
        const list = (Array.isArray(values) ? values : []).filter((item): item is string => typeof item === 'string')
        const selected = list.filter((item) => labels.has(item))
        const typed = list.filter((item) => !labels.has(item)).join('\n').slice(0, MAX_TEXT_CHARS)
        return {
          id, selected, ...(typed === '' ? {} : { custom: typed }),
          ...(entry.previewHashes.has(id) ? { preview_data_sha256: answer?.['preview_data_sha256'] } : {}),
        }
      })
      entry.resolve({ answers })
      return true
    },
  }
}
