import assert from 'node:assert/strict'
import { test } from 'node:test'
import { relayEvents, EVENT_METHOD } from '../lib/events.js'

/** A host context that records listeners so a test can fire host events. */
function host() {
  const listeners = new Map()
  const sent = []
  const agents = new Map()
  const ctx = {
    on: (name, listener, options) => listeners.set(name, { listener, options }),
    get: name => (name === 'agents' ? agents : undefined),
  }
  const turns = relayEvents(ctx, frame => sent.push(frame))
  const stream = (session, frame) => listeners.get('agent/assistant-stream').listener({ agent: { session: { header: { id: session } } }, frame })
  const event = (session, type, data) => listeners.get('session/event').listener({ header: { id: session } }, { type, data })
  const claim = (session, id) => listeners.get('agent/inbox/claimed').listener({ agent: { session: { header: { id: session } } }, message: { id } })
  return { listeners, sent, stream, event, agents, turns, claim }
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

test('steering reaches only a running turn and reports delivery once', () => {
  const { sent, event, agents, turns, claim } = host()
  const steered = []
  const agent = { status: 'running', steer: message => steered.push(message) }
  let next = 0
  const create = input => ({ id: `message-${++next}`, ...input })
  assert.throws(() => turns.steer('s', 'go left', create), /no running agent/)
  agents.set('s', agent)
  // An agent without an open turn would start one that no prompt waits for.
  assert.throws(() => turns.steer('s', 'go left', create), /no turn is running/)
  event('s', 'turn/start', {})
  assert.throws(() => turns.steer('s', '   ', create), /empty or too long/)
  assert.throws(() => turns.steer('s', 'x'.repeat(32 * 1024 + 1), create), /empty or too long/)
  agent.status = 'idle'
  assert.throws(() => turns.steer('s', 'go left', create), /no turn is running/)
  agent.status = 'running'
  assert.deepEqual(turns.steer('s', 'go left', create), { messageId: 'message-1' })
  assert.deepEqual(steered[0].content, [{ type: 'text', text: 'go left' }])
  assert.deepEqual(steered[0].source, { kind: 'user' })
  claim('s', 'someone-elses-message')
  claim('other', 'message-1')
  claim('s', 'message-1')
  claim('s', 'message-1')
  assert.deepEqual(sent.map(frame => frame.params.event), [{ type: 'steer', id: 'message-1', state: 'delivered' }])
  // A message the turn never claimed is not reported as delivered afterwards.
  turns.steer('s', 'go right', create)
  event('s', 'turn/end', {})
  claim('s', 'message-2')
  assert.equal(sent.length, 1)
  assert.throws(() => turns.steer('s', 'late', create), /no turn is running/)
  // A host refusal leaves nothing pending.
  event('s', 'turn/start', {})
  agent.steer = () => { throw new Error('host refused') }
  assert.throws(() => turns.steer('s', 'again', create), /host refused/)
  claim('s', 'message-3')
  assert.equal(sent.length, 1)
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
