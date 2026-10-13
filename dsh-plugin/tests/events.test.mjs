import assert from 'node:assert/strict'
import { test } from 'node:test'
import { relayEvents, EVENT_METHOD } from '../lib/events.js'

/** A host context that records listeners so a test can fire host events. */
function host() {
  const listeners = new Map()
  const sent = []
  const ctx = { on: (name, listener, options) => listeners.set(name, { listener, options }) }
  relayEvents(ctx, frame => sent.push(frame))
  const stream = (session, frame) => listeners.get('agent/assistant-stream').listener({ agent: { session: { header: { id: session } } }, frame })
  const event = (session, type, data) => listeners.get('session/event').listener({ header: { id: session } }, { type, data })
  return { listeners, sent, stream, event }
}
const chunk = type => ({ type: 'chunk', chunk: { type, text: 'PRIVATE model output' } })

test('a stage is announced once per change and never carries model output', () => {
  const { listeners, sent, stream, event } = host()
  assert.equal(listeners.get('agent/assistant-stream').options.global, true)
  stream('s', { type: 'start', turn: 't', step: 1 })
  for (const type of ['reasoning-delta', 'reasoning-delta', 'usage', 'text-delta', 'text-delta', 'tool-call-delta']) stream('s', chunk(type))
  stream('s', { type: 'end' })
  stream('other', chunk('text-delta'))
  // The next request of the same turn waits again; a new turn starts clean.
  stream('s', { type: 'start' })
  event('s', 'turn/end', {})
  stream('s', { type: 'start' })
  assert.deepEqual(sent.map(frame => [frame.method, frame.params.sessionId, frame.params.event.stage]), [
    [EVENT_METHOD, 's', 'waiting'], [EVENT_METHOD, 's', 'thinking'], [EVENT_METHOD, 's', 'response'],
    [EVENT_METHOD, 'other', 'response'], [EVENT_METHOD, 's', 'waiting'], [EVENT_METHOD, 's', 'waiting'],
  ])
  assert.ok(!JSON.stringify(sent).includes('PRIVATE'))
})

test('retries carry counts and the host failure code, compaction its outcome', () => {
  const { sent, event } = host()
  event('s', 'llm/retry', { retry: 1, maxRetries: 2, delayMs: 500, provider: 'p', failure: { code: 'rate_limit', message: 'PRIVATE key sk-123' } })
  event('s', 'llm/retry', { retry: 2, delayMs: 1000, failure: { code: 'has spaces and PRIVATE text' } })
  event('s', 'llm/retry', { retry: -1 })
  event('s', 'compaction/start', { compactionId: 'c1', turn: 't' })
  event('s', 'compaction/end', { compactionId: 'c1', turn: 't' })
  event('s', 'compaction/end', { compactionId: 'c2', error: [{ message: 'PRIVATE' }] })
  event('s', 'compaction/summary', { compactionId: 'c1', summary: 'PRIVATE' })
  event('s', 'compaction/start', {})
  assert.deepEqual(sent.map(frame => frame.params.event), [
    { type: 'retry', retry: 1, maxRetries: 2, delayMs: 500, code: 'rate_limit' },
    { type: 'retry', retry: 2, delayMs: 1000 },
    { type: 'compaction', id: 'c1', state: 'start' },
    { type: 'compaction', id: 'c1', state: 'end' },
    { type: 'compaction', id: 'c2', state: 'failed' },
  ])
  assert.ok(!JSON.stringify(sent).includes('PRIVATE'))
})

test('a todo list is relayed whole, bounded, and without malformed items', () => {
  const { sent, event, stream } = host()
  event('s', 'todo/write', { todos: [
    { content: 'scan sources', status: 'completed' },
    { content: 'x'.repeat(2000), status: 'in_progress' },
    { content: 'bad status', status: 'done' },
    { content: 7, status: 'pending' },
    ...Array.from({ length: 200 }, (_, index) => ({ content: `item ${index}`, status: 'pending' })),
  ] })
  event('s', 'todo/write', { todos: 'not a list' })
  event('s', 'assistant/message', { message: { content: [{ type: 'text', text: 'PRIVATE' }] } })
  stream(undefined, chunk('text-delta'))
  assert.equal(sent.length, 1)
  const { sessionId, event: relayed } = sent[0].params
  assert.equal(sessionId, 's')
  assert.equal(relayed.type, 'todos')
  assert.equal(relayed.todos.length, 98)
  assert.deepEqual(relayed.todos[0], { content: 'scan sources', status: 'completed' })
  assert.equal(relayed.todos[1].content.length, 512)
  assert.ok(relayed.todos.every(todo => ['pending', 'in_progress', 'completed'].includes(todo.status)))
  // An emptied list is a fact too: the agent cleared its todos.
  event('s', 'todo/write', { todos: [] })
  assert.deepEqual(sent[1].params.event, { type: 'todos', todos: [] })
})
