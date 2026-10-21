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
  assert.equal(params.questions[0].multiSelect, false)
  assert.equal(params.questions[1].options, undefined)
  assert.equal(params.questions[1].multiSelect, undefined)
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


test('29 item decisions travel in one request and retain individual feedback', async () => {
  const { ask, sent, questions, agent } = host()
  const items = Array.from({ length: 29 }, (_, index) => ({
    ...review, id: `component-${index}`, header: `Item ${index + 1}`,
    options: [{ label: 'Accept', description: 'Retain for validation' }, { label: 'Defer', description: 'Keep pending' }],
  }))
  const response = ask({ agent, questions: items })
  assert.equal(sent.length, 1)
  assert.equal(sent[0].params.questions.length, 29)
  const answers = Object.fromEntries(items.map(({ id }, index) => [id, {
    answers: [index % 3 === 0 ? 'Accept' : index % 3 === 1 ? 'move X +0.25 m' : 'Defer'],
  }]))
  questions.settle({ id: sent[0].id, result: { answers } })
  const result = await response
  assert.equal(result.answers.length, 29)
  assert.deepEqual(result.answers[0], { id: 'component-0', selected: ['Accept'] })
  assert.deepEqual(result.answers[1], { id: 'component-1', selected: [], custom: 'move X +0.25 m' })
  assert.deepEqual(result.answers[28], { id: 'component-28', selected: [], custom: 'move X +0.25 m' })
})


test('64 question boundary is forwarded whole; a larger batch is passed on whole', async () => {
  const { ask, sent, questions, agent } = host()
  const items = Array.from({ length: 64 }, (_, index) => ({ ...review, id: `item-${index}` }))
  const response = ask({ agent, questions: items })
  assert.equal(sent[0].params.questions.length, 64)
  const answers = Object.fromEntries(items.map(({ id }) => [id, { answers: ['Approve'] }]))
  questions.settle({ id: sent[0].id, result: { answers } })
  assert.equal((await response).answers.length, 64)
  assert.equal(ask({ agent, questions: [...items, { ...review, id: 'item-64' }] }), 'passed on')
  assert.equal(sent.length, 1)
})


test('question identity is never shortened before matching the reply', async () => {
  const { ask, sent, questions, agent } = host()
  const id = `component.${'x'.repeat(180)}`
  const response = ask({ agent, questions: [{ ...review, id }] })
  assert.equal(sent[0].params.questions[0].id, id)
  questions.settle({ id: sent[0].id, result: { answers: { [id]: { answers: ['Approve'] } } } })
  assert.deepEqual((await response).answers, [{ id, selected: ['Approve'] }])
})

test('previews and their answer hashes retain each question identity in a mixed batch', async () => {
  const { ask, sent, questions, agent } = host()
  const preview = {
    html_path: `D:/项目/${'x'.repeat(6000)}/review.html`,
    png_path: 'D:/项目/review.png', data_sha256: 'a'.repeat(64),
  }
  const second = { html_path: 'other/review.html', png_path: 'other/review.png', data_sha256: 'b'.repeat(64) }
  const response = ask({ agent, questions: [
    { ...review, preview },
    { id: 'note', header: 'Note', question: 'Anything else?' },
    { ...review, id: 'second', multiSelect: true, preview: second },
  ] })
  assert.deepEqual(sent[0].params.questions[0].preview, preview)
  assert.equal(Object.hasOwn(sent[0].params.questions[1], 'preview'), false)
  assert.deepEqual(sent[0].params.questions[2].preview, second)
  assert.equal(questions.settle({ id: sent[0].id, result: { answers: {
    second: { answers: ['Approve', 'Keep planning', 'move X'], preview_data_sha256: second.data_sha256 },
    note: { answers: ['looks good'] },
    'plan-review': { answers: ['Approve'], preview_data_sha256: preview.data_sha256 },
  } } }), true)
  assert.deepEqual(await response, { answers: [
    { id: 'plan-review', selected: ['Approve'], preview_data_sha256: preview.data_sha256 },
    { id: 'note', selected: [], custom: 'looks good' },
    { id: 'second', selected: ['Approve', 'Keep planning'], custom: 'move X', preview_data_sha256: second.data_sha256 },
  ] })
})

test('preview answers reject missing, invalid and mismatched hashes without synthesizing one', async () => {
  for (const reply of [
    undefined,
    { answers: ['Approve'] },
    ...[null, '', 123, 'b'.repeat(64), 'A'.repeat(64), `${'a'.repeat(64)} `].map(hash => ({
      answers: ['Approve'], preview_data_sha256: hash,
    })),
  ]) {
    const { ask, sent, questions, agent } = host()
    const response = ask({ agent, questions: [{ ...review, preview: {
      html_path: 'review.html', png_path: 'review.png', data_sha256: 'a'.repeat(64),
    } }] })
    const rejected = assert.rejects(response, /missing or mismatched preview_data_sha256.*plan-review/)
    assert.equal(questions.settle({ id: sent[0].id, result: { answers: reply === undefined ? {} : { 'plan-review': reply } } }), true)
    await rejected
    assert.equal(questions.settle({ id: sent[0].id, result: { answers: {
      'plan-review': { answers: ['Approve'], preview_data_sha256: 'a'.repeat(64) },
    } } }), false)
  }
})

test('preview hashes cannot be swapped between questions or replaced after dispatch', async () => {
  const { ask, sent, questions, agent } = host()
  const first = { html_path: 'a.html', png_path: 'a.png', data_sha256: 'a'.repeat(64) }
  const second = { html_path: 'b.html', png_path: 'b.png', data_sha256: 'b'.repeat(64) }
  const response = ask({ agent, questions: [
    { ...review, preview: first }, { ...review, id: 'second', preview: second },
  ] })
  const rejected = assert.rejects(response, /missing or mismatched preview_data_sha256.*plan-review/)
  first.data_sha256 = second.data_sha256
  questions.settle({ id: sent[0].id, result: { answers: {
    'plan-review': { answers: ['Approve'], preview_data_sha256: second.data_sha256 },
    second: { answers: ['Approve'], preview_data_sha256: 'a'.repeat(64) },
  } } })
  await rejected
})
