import assert from 'node:assert/strict'
import { test } from 'node:test'
import { answerQuestions, ASK_METHOD } from '../lib/questions.js'

function host() {
  let handler
  const sent = []
  const ctx = { on: (name, listener) => { assert.equal(name, 'user-questions/request'); handler = listener } }
  const dismissed = message => Object.assign(new Error(message), { code: 'ASK_CANCELLED' })
  const questions = answerQuestions(ctx, frame => sent.push(frame), dismissed)
  const agent = { session: { header: { id: 's' } } }
  return { ask: (request, pass = () => 'passed on') => handler(request, pass), sent, questions, agent }
}
const review = {
  id: 'plan-review', header: 'Plan review', question: 'Approve this plan?', detail: '# Plan\n1. scan',
  options: [{ label: 'Approve', description: 'Leave plan mode' }, { label: 'Keep planning', description: 'Stay' }],
}

test('a question becomes one request to the TUI and its reply is converted for the host', async () => {
  const { ask, sent, questions, agent } = host()
  const answer = ask({ agent, questions: [review, { id: 'note', header: 'Note', question: 'Anything else?' }] })
  assert.equal(sent.length, 1)
  const { id, method, params } = sent[0]
  assert.equal(method, ASK_METHOD)
  assert.equal(params.sessionId, 's')
  assert.equal(params.questions[0].question, 'Approve this plan?\n\n# Plan\n1. scan')
  assert.deepEqual(params.questions[0].options.map(option => option.label), ['Approve', 'Keep planning'])
  assert.equal(params.questions[1].options, undefined)
  // Frames that are not the reply are left for the ACP server.
  assert.equal(questions.settle({ id: 'other', result: {} }), false)
  assert.equal(questions.settle({ id, method: 'something', params: {} }), false)
  assert.equal(questions.settle({ id, result: { answers: { 'plan-review': { answers: ['Approve'] }, note: { answers: ['ship it', 'today'] } } } }), true)
  assert.deepEqual(await answer, { answers: [
    { id: 'plan-review', selected: ['Approve'] },
    { id: 'note', selected: [], custom: 'ship it\ntoday' },
  ] })
  assert.equal(questions.settle({ id, result: {} }), false)
})

test('typed feedback, dismissal and withdrawal each reach the host as what they are', async () => {
  const { ask, sent, questions, agent } = host()
  const feedback = ask({ agent, questions: [review] })
  questions.settle({ id: sent[0].id, result: { answers: { 'plan-review': { answers: ['add tests first'] } } } })
  assert.deepEqual(await feedback, { answers: [{ id: 'plan-review', selected: [], custom: 'add tests first' }] })

  const declined = ask({ agent, questions: [review] })
  questions.settle({ id: sent[1].id, error: { code: -32000, message: 'declined' } })
  await assert.rejects(declined, error => error.code === 'ASK_CANCELLED')

  const controller = new AbortController()
  const withdrawn = ask({ agent, questions: [review], signal: controller.signal })
  controller.abort(new Error('turn interrupted'))
  await assert.rejects(withdrawn, /turn interrupted/)
  assert.deepEqual(sent[3].params.event, { type: 'ask', id: sent[2].id, state: 'withdrawn' })
  assert.equal(questions.settle({ id: sent[2].id, result: { answers: {} } }), false)

  // Questions that belong to no session, or to none at all, are passed on.
  assert.equal(ask({ questions: [review] }), 'passed on')
  assert.equal(ask({ agent, questions: [] }), 'passed on')
  assert.equal(sent.length, 4)
})
