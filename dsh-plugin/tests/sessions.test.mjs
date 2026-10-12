import assert from 'node:assert/strict'
import { test } from 'node:test'
import { readHistory, HISTORY_MESSAGE_LIMIT, HISTORY_TEXT_LIMIT } from '../lib/sessions.js'

const query = events => ({ readSurface: async () => ({ events }) })
const assistant = (id, content, turn = 'turn-1') => ({ type: 'assistant/message', data: { turn, message: { id, content } } })
const call = (id, command, turn = 'turn-1') => assistant(`message-${id}`, [{ type: 'tool-call', id, name: 'pwsh', arguments: JSON.stringify({ command }) }], turn)
const result = (id, isError = false, turn = 'turn-1') => ({ type: 'tool/result', data: { turn, message: { content: [{ type: 'tool-result', toolCallId: id, isError, content: [{ type: 'text', text: 'raw output is not history text' }] }] } } })

test('restoring a conversation keeps reasoning and tool lifecycles in server order', async () => {
  const history = await readHistory(query([
    { type: 'user/message', data: { source: { kind: 'user' }, content: [{ type: 'text', text: 'question' }] } },
    assistant('shared', [{ type: 'reasoning', text: 'thought' }, { type: 'text', text: 'plan' }]),
    call('tool-1', 'echo probe'),
    result('tool-1'),
    assistant('answer', [{ type: 'text', text: 'answer' }]),
  ]), 'session')
  assert.deepEqual(history.messages.map(m => [m.role, m.text]), [
    ['user', 'question'], ['reasoning', 'thought'], ['assistant', 'plan'], ['tool', '▸ done · echo probe'], ['assistant', 'answer'],
  ])
  assert.equal(history.messages[1].id, 'shared')
  assert.equal(history.messages[2].id, 'shared')
  assert.equal(history.messages[3].id, 'tool-1')
  assert.equal(history.messages[3].turnId, 'turn-1')
})

test('tool outcomes use the exact turn and call id, including unknown outcomes', async () => {
  const history = await readHistory(query([
    call('same', 'one', 'one'), call('same', 'two', 'two'), result('same', true, 'two'),
    { type: 'turn/end', data: { turn: 'one' } }, result('missing', false, 'other'),
  ]), 'session')
  assert.deepEqual(history.messages.map(m => m.text), ['▸ unknown · one', '▸ failed · two'])
})

test('history bounds tool and reasoning text and still excludes injected context', async () => {
  const events = [
    { type: 'user/message', data: { source: { kind: 'context' }, content: [{ type: 'text', text: 'injected' }] } },
    ...Array.from({ length: HISTORY_MESSAGE_LIMIT + 1 }, (_, i) => assistant(String(i), [{ type: 'reasoning', text: 'r'.repeat(HISTORY_TEXT_LIMIT + 1) }])),
  ]
  const history = await readHistory(query(events), 'session')
  assert.equal(history.messages.length, HISTORY_MESSAGE_LIMIT)
  assert.equal(history.omitted, 1)
  assert.ok(history.messages.every(m => m.role === 'reasoning' && m.truncated && m.text.length === HISTORY_TEXT_LIMIT))
})


test('current model surface restores tool-call blocks without raw tool/call events', async () => {
  const history = await readHistory(query([
    assistant('plan', [{ type: 'reasoning', text: 'Plan' }, { type: 'tool-call', id: 'surface-call', name: 'read', arguments: JSON.stringify({ file_path: 'src/main.rs' }) }]),
    result('surface-call'),
    assistant('answer', [{ type: 'text', text: 'Done' }]),
  ]), 'session')
  assert.deepEqual(history.messages.map(m => [m.role, m.text]), [
    ['reasoning', 'Plan'], ['tool', '▸ done · read src/main.rs'], ['assistant', 'Done'],
  ])
  assert.equal(history.messages[1].id, 'surface-call')
})


test('recovered unknown tool outcomes stay explicit and clipped commands stay marked', async () => {
  const recovered = result('recovered', true)
  recovered.data.error = { code: 'TOOL_OUTCOME_UNKNOWN' }
  const history = await readHistory(query([
    call('recovered', 'probe'), recovered,
    call('long', '界'.repeat(HISTORY_TEXT_LIMIT)), result('long'),
  ]), 'session')
  assert.equal(history.messages[0].text, '▸ ended, outcome unknown · probe')
  assert.ok(history.messages[1].text.startsWith('▸ done · '))
  assert.ok(history.messages[1].truncated)
  assert.ok(history.messages[1].text.length <= HISTORY_TEXT_LIMIT)
})


test('legacy saved tool results keep their top-level outcome and call identity', async () => {
  const history = await readHistory(query([
    call('legacy-success', 'ok', 1),
    { type: 'tool/result', data: { turn: 1, message: { role: 'tool', toolCallId: 'legacy-success', content: [{ type: 'text', text: 'raw output' }], isError: false } } },
    call('legacy-failure', 'fail', 2),
    { type: 'tool/result', data: { turn: 2, message: { role: 'tool', toolCallId: 'legacy-failure', content: [], isError: true } } },
  ]), 'session')
  assert.deepEqual(history.messages.map(m => m.text), ['▸ done · ok', '▸ failed · fail'])
})
