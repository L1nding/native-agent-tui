import assert from 'node:assert/strict'
import { test } from 'node:test'
import { relayEvents, EVENT_METHOD } from '../lib/events.js'
import { apply } from '../lib/index.js'

let contextSource

test('installer restores every original after staged or last commit failures and can retry', async () => {
  const { commitRepairs } = await import('../../scripts/install-dsh-context.mjs')
  for (const size of [2, 3, 4]) {
  for (const fault of ['stage-throw', 'stage-truncate', 'commit-throw', 'commit-truncate']) {
    const paths = Array.from({ length: size }, (_, index) => `host/${index}.js`)
    const last = paths.at(-1)
    const originals = paths.map((_, index) => Buffer.from([65 + index, 0, 255 - index]))
    const files = new Map(paths.map((path, index) => [path, originals[index]]))
    let injected = false
    const io = {
      readFileSync(path) {
        if (!files.has(path)) throw Object.assign(new Error('missing'), { code: 'ENOENT' })
        return Buffer.from(files.get(path))
      },
      writeFileSync(path, content, options) {
        if (options?.flag === 'wx' && files.has(path)) throw new Error('already exists')
        if (!injected && path.startsWith(last) && path.endsWith('.tmp') && fault.startsWith('stage')) {
          injected = true
          files.set(path, Buffer.from('partial'))
          if (fault === 'stage-throw') throw new Error('second staged write failed')
          return
        }
        files.set(path, Buffer.from(content))
      },
      renameSync(from, to) {
        if (!injected && to === last && from.endsWith('.tmp') && fault.startsWith('commit')) {
          injected = true
          if (fault === 'commit-truncate') files.set(to, Buffer.from('partial'))
          throw new Error('second host replacement failed')
        }
        files.set(to, io.readFileSync(from)); files.delete(from)
      },
      unlinkSync(path) {
        if (!files.delete(path)) throw Object.assign(new Error('missing'), { code: 'ENOENT' })
      },
    }
    const plans = paths.map((path, index) => ({ path, originalBytes: originals[index], changed: true, content: `patched-${index}` }))
    assert.throws(() => commitRepairs(plans, { io }), /failed/)
    for (const [index, path] of paths.entries()) assert.deepEqual(io.readFileSync(path), originals[index], fault)
    assert.equal(files.size, size, 'staging artifacts are removed after rollback')
    assert.throws(() => commitRepairs(plans, { io, check: true }), /not installed/)
    commitRepairs(plans, { io })
    for (const [index, path] of paths.entries()) assert.equal(io.readFileSync(path).toString(), `patched-${index}`)
    commitRepairs(plans, { io, check: true })
    assert.equal(files.size, size)
  }
  }
})

async function contextModule() {
  if (process.env.DSH_CONTEXT_SOURCE !== '1') return import('../lib/context.js')
  if (!contextSource) {
    const { readFileSync } = await import('node:fs')
    const ts = (await import('typescript')).default
    const source = ts.transpileModule(readFileSync(new URL('../src/context.ts', import.meta.url), 'utf8'), {
      compilerOptions: { target: ts.ScriptTarget.ES2023, module: ts.ModuleKind.ESNext },
    }).outputText
    contextSource = `data:text/javascript;base64,${Buffer.from(source + '\n//# sourceURL=zf-context-offline.js').toString('base64')}`
  }
  return import(contextSource)
}

async function hostModule(name) {
  const { createRequire } = await import('node:module')
  const { pathToFileURL } = await import('node:url')
  const require = createRequire(process.env.DSH_CONTEXT_HOST_ENTRY)
  if (process.env.DSH_CONTEXT_SOURCE !== '1') return import(pathToFileURL(require.resolve(name)).href)
  await contextModule()
  const { prepareRepairs } = await import('../../scripts/install-dsh-context.mjs')
  const { existsSync, readFileSync } = await import('node:fs')
  const { join, resolve } = await import('node:path')
  const plan = prepareRepairs(process.env.DSH_CONTEXT_HOST_ENTRY, contextSource).find(plan => plan.path === require.resolve(name))
  let source = plan.content.split('\n// zf-context-repair-original:')[0].replace(/from (["'])([^"']+)\1/g, (whole, _quote, specifier) => {
    if (/^(node:|file:|data:)/.test(specifier)) return whole
    let path
    try { path = require.resolve(specifier) } catch (error) {
      if (error.code !== 'ERR_PACKAGE_PATH_NOT_EXPORTED') throw error
      // pi-ai 仅声明 import 条件出口；内存模块沿用原宿主的这个出口。
      const parts = specifier.split('/'), packageName = parts.slice(0, specifier.startsWith('@') ? 2 : 1).join('/')
      const root = require.resolve.paths(packageName).map(directory => join(directory, packageName)).find(directory => existsSync(join(directory, 'package.json')))
      const subpath = '.' + specifier.slice(packageName.length)
      const exports = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).exports
      for (const [key, value] of Object.entries(exports)) {
        const prefix = key.split('*')[0]
        if (key === subpath || (key.endsWith('*') && subpath.startsWith(prefix))) {
          path = resolve(root, value.import.replace('*', subpath.slice(prefix.length))); break
        }
      }
      if (!path) throw error
    }
    return `from ${JSON.stringify(pathToFileURL(path).href)}`
  })
  if (name === '@deepseek-ai/dsh-acp') source += '\nexport { AcpSession as ZfTestAcpSession };\n'
  if (name === '@deepseek-ai/dsh-mcp-client') source += '\nexport { startConnection as ZfTestStartConnection };\n'
  return import(`data:text/javascript;base64,${Buffer.from(source + `\n//# sourceURL=zf-offline-${name.split('/').at(-1)}.js`).toString('base64')}`)
}

test('expired HTTP MCP sessions retire once without replaying calls and dispose cancels reconnect', {
  skip: !process.env.DSH_CONTEXT_HOST_ENTRY || process.env.DSH_CONTEXT_SOURCE !== '1',
}, async () => {
  const { ZfTestStartConnection } = await hostModule('@deepseek-ai/dsh-mcp-client')
  const savedFetch = globalThis.fetch
  const definitions = new Map(), requests = [], waiting = []
  let opened = 0, fault, hold = false
  globalThis.fetch = async (_url, options) => {
    if (options.method === 'GET') return new Response('', { status: 405 })
    const body = JSON.parse(options.body), session = new Headers(options.headers).get('mcp-session-id')
    requests.push({ method: body.method, session, args: body.params?.arguments })
    const reply = result => new Response(JSON.stringify({ jsonrpc: '2.0', id: body.id, result }), {
      headers: { 'content-type': 'application/json', 'mcp-session-id': `session-${opened}` },
    })
    if (body.method === 'server/discover') return new Response(JSON.stringify({ jsonrpc: '2.0', id: body.id,
      error: { code: -32601, message: 'Method not found' } }), { status: 400, headers: { 'content-type': 'application/json' } })
    if (body.method === 'initialize') {
      opened += 1
      return reply({ protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'offline', version: '1' } })
    }
    if (body.method === 'notifications/initialized') return new Response(null, { status: 202 })
    if (body.method === 'tools/list') return reply({ tools: [{ name: 'inspect', inputSchema: { type: 'object' } }] })
    if (body.method === 'notifications/cancelled') return new Response(null, { status: 202 })
    assert.equal(body.method, 'tools/call')
    if (hold) await new Promise((resolve, reject) => {
      const abort = () => reject(new DOMException('fixture request cancelled', 'AbortError'))
      options.signal?.addEventListener('abort', abort, { once: true })
      waiting.push(() => { options.signal?.removeEventListener('abort', abort); resolve() })
    })
    if (fault) return new Response(fault.raw ?? JSON.stringify({ jsonrpc: '2.0', id: 'server-error', error: { code: fault.code ?? -32600, message: fault.message } }), { status: fault.status })
    return reply({ content: [{ type: 'text', text: 'offline success' }] })
  }
  const ctx = { tools: { register(definition) {
    definitions.set(definition.name, definition)
    return () => { if (definitions.get(definition.name) === definition) definitions.delete(definition.name) }
  } }, logger: { info() {}, warn() {}, error() {} } }
  const config = { transport: 'streamable-http', serverName: 'offline', url: 'http://offline.invalid/mcp', toolCallTimeoutMs: 2000 }
  const policy = { enabled: true, initialDelayMs: 10, maxDelayMs: 20, maxAttempts: 2 }
  const connection = ZfTestStartConnection(ctx, config, policy)
  const current = () => definitions.get('mcp__offline__inspect')
  const execute = (definition, value, signal = new AbortController().signal) => definition.execute({ value }, { signal })
  try {
    assert.deepEqual(await connection.ready, {})
    const original = current()
    for (const nonSession of [
      { status: 404, message: 'Tool not found' }, { status: 500, message: 'Session not found' },
      { status: 404, code: -32601, message: 'Session not found' }, { status: 404, raw: 'Session not found' },
    ]) {
      fault = nonSession
      await assert.rejects(execute(original, 'negative'), /Error POSTing/)
      assert.equal(opened, 1)
    }
    const aborted = new AbortController(); aborted.abort()
    await assert.rejects(execute(original, 'cancelled', aborted.signal))
    assert.equal(opened, 1)
    hold = true
    const during = new AbortController()
    const cancelledCall = execute(original, 'cancel-in-flight', during.signal)
    const cancelledResult = assert.rejects(cancelledCall)
    while (waiting.length < 1) await new Promise(resolve => setTimeout(resolve, 1))
    during.abort()
    await cancelledResult
    waiting.splice(0).forEach(resolve => resolve()); hold = false
    assert.equal(opened, 1, 'caller cancellation does not reconnect a healthy generation')
    fault = { status: 404, message: 'Session not found' }; hold = true
    const a = execute(original, 'read'), b = execute(original, 'write')
    const results = Promise.allSettled([a, b])
    while (waiting.length < 2) await new Promise(resolve => setTimeout(resolve, 1))
    hold = false; waiting.splice(0).forEach(resolve => resolve())
    const failures = await results
    for (const result of failures) {
      assert.equal(result.status, 'rejected')
      assert.match(result.reason.message, /Session not found/)
      assert.match(result.reason.message, /not replayed|未重放/)
    }
    fault = undefined
    for (let i = 0; i < 100 && current() === original; i++) await new Promise(resolve => setTimeout(resolve, 5))
    assert.equal(opened, 2, 'concurrent 404s create one replacement session')
    assert.notEqual(current(), original)
    await assert.rejects(execute(original, 'retired'), /disconnected|retired/)
    assert.ok((await execute(current(), 'new')).content.length)
    assert.equal(requests.filter(request => request.args?.value === 'read').length, 1)
    assert.equal(requests.filter(request => request.args?.value === 'write').length, 1)
    assert.equal(requests.find(request => request.args?.value === 'new').session, 'session-2')
    fault = { status: 404, message: 'Session not found' }
    await assert.rejects(execute(current(), 'last'), /not replayed|未重放/)
    await connection.dispose()
    await new Promise(resolve => setTimeout(resolve, 40))
    assert.equal(opened, 2, 'dispose cancels queued reconnect')
    assert.equal(definitions.size, 0)
    fault = undefined
    const nextConnection = ZfTestStartConnection(ctx, config, policy)
    try {
      assert.deepEqual(await nextConnection.ready, {})
      const beforeClose = opened
      fault = { status: 404, message: 'Session not found' }; hold = true
      const inFlight = Promise.allSettled([execute(current(), 'dispose-in-flight')])
      while (waiting.length < 1) await new Promise(resolve => setTimeout(resolve, 1))
      await nextConnection.dispose()
      hold = false; waiting.splice(0).forEach(resolve => resolve())
      assert.equal((await inFlight)[0].status, 'rejected')
      await new Promise(resolve => setTimeout(resolve, 40))
      assert.equal(opened, beforeClose, 'late 404 after close cannot reconnect or revive registrations')
      assert.equal(definitions.size, 0)
    } finally { await nextConnection.dispose() }
  } finally { await connection.dispose(); globalThis.fetch = savedFetch }
})

test('gateway concurrency failures use bounded cancellable retries without widening generic errors', {
  skip: !process.env.DSH_CONTEXT_HOST_ENTRY,
}, async () => {
  const { PiAiAdapter } = await hostModule('@deepseek-ai/dsh-llm-pi-ai')
  const { createRequire } = await import('node:module')
  const { pathToFileURL } = await import('node:url')
  const require = createRequire(process.env.DSH_CONTEXT_HOST_ENTRY)
  const { apply: retryPlugin } = await import(pathToFileURL(require.resolve('@deepseek-ai/dsh-llm-retry')).href)
  const adapter = Object.create(PiAiAdapter.prototype)
  let message, providerCalls = 0
  const snapshot = {
    profiles: new Map([['hi', { streamIdleTimeoutMs: 10000 }]]),
    models: { async *streamSimple() {
      providerCalls += 1
      yield { type: 'error', error: { content: [], stopReason: 'error', errorMessage: message,
        usage: { input: 0, output: 0, totalTokens: 0, cacheRead: 0, cacheWrite: 0 } } }
    } },
  }
  adapter.current = () => snapshot
  adapter.modelOf = () => ({ id: 'gpt-6-luna', provider: 'hi', contextWindow: 272000, input: ['text'] })
  adapter.config = { resolveApiKey: async () => 'offline-fixture' }
  let projection, recover, teardown, state = {}, cancelOnRetry = false
  const records = []
  let controller
  const agent = { session: { append(type, data) {
    records.push({ type, data })
    state = projection.apply(state, { type, data })
    if (cancelOnRetry && type === 'llm/retry') controller.abort()
  } } }
  retryPlugin({
    sessionProjections: { register(value) { projection = value }, stateOf: () => state },
    on(name, listener) { assert.equal(name, 'agent/request-error'); recover = listener; return () => {} },
    effect(dispose) { teardown = dispose },
  }, {}, { random: () => .5 })
  const retryPolicy = { mode: 'normal', maxRetries: 1, retryableCodes: ['RATE_LIMIT', 'SERVER', 'TIMEOUT', 'TRANSPORT', 'EMPTY_RESPONSE'],
    initialDelayMs: 1, maxDelayMs: 1, jitterRatio: 0 }
  const fixtures = [
    ['gateway_concurrency_limit: Concurrency limit exceeded for user, please retry later', 'RATE_LIMIT'],
    ['gateway_concurrency_limit', 'RATE_LIMIT'],
    ['Concurrency limit exceeded for user, please retry later', 'RATE_LIMIT'],
    ['generic provider failure', 'PI_AI_ERROR'],
    ['Concurrency limit exceeded', 'PI_AI_ERROR'],
    ['Concurrency limit exceeded for users', 'PI_AI_ERROR'],
    ['gateway_concurrency_limit_other', 'PI_AI_ERROR'],
    ['not_gateway_concurrency_limit', 'PI_AI_ERROR'],
    ['401 gateway_concurrency_limit', 'AUTH'],
  ]
  try {
    for (const [text, code] of fixtures) {
      message = text
      controller = new AbortController()
      state = {}; records.length = 0
      let finish
      for await (const chunk of adapter.streamWithSnapshot({ provider: 'hi', model: 'gpt-6-luna',
        messages: [{ role: 'user', content: [{ type: 'text', text: 'offline classifier fixture' }] }], maxTokens: 16384 }, snapshot)) {
        if (chunk.type === 'finish') finish = chunk.reason
      }
      assert.equal(finish.kind, 'error')
      assert.equal(finish.failure.code, code, text)
      assert.equal(finish.failure.message, text)
      const request = { agent, turn: 1, step: 1, provider: 'hi', failure: finish.failure, retryPolicy, signal: controller.signal }
      const next = async () => ({ kind: 'unhandled' })
      const decision = await recover(request, next)
      if (code === 'RATE_LIMIT') {
        assert.deepEqual(decision, { kind: 'retry' })
        assert.deepEqual(records.map(record => record.type), ['llm/retry', 'llm/retry-started'])
        assert.equal(records[0].data.maxRetries, 1)
        assert.deepEqual(await recover(request, next), { kind: 'unhandled' }, 'retry ceiling remains effective')
        state = {}; records.length = 0; cancelOnRetry = true
        assert.equal(await recover(request, next), undefined)
        assert.deepEqual(records.map(record => record.type), ['llm/retry'], 'cancellation prevents retry dispatch')
        cancelOnRetry = false
      } else {
        assert.deepEqual(decision, { kind: 'unhandled' })
        assert.deepEqual(records, [])
      }
    }
    assert.equal(providerCalls, fixtures.length, 'only the in-memory provider fixture is invoked once per case')
  } finally { await teardown()() }
})

test('adapter byte offload cannot discard a newly delivered unobserved batch', {
  skip: !process.env.DSH_CONTEXT_HOST_ENTRY,
}, async () => {
  const { PiAiAdapter } = await hostModule('@deepseek-ai/dsh-llm-pi-ai')
  const adapter = Object.create(PiAiAdapter.prototype)
  let providerCalls = 0
  const profile = { requestImagePixelBudget: 4194304, requestImageMaxBytes: 1048576,
    maxRequestImageBytes: 20 * 1048576, streamIdleTimeoutMs: 10000 }
  const snapshot = { profiles: new Map([['hi', profile]]), models: { streamSimple() { providerCalls += 1; throw new Error('NO_MODEL_CALLS') } } }
  adapter.current = () => snapshot
  adapter.modelOf = () => ({ id: 'gpt-6-luna', provider: 'hi', contextWindow: 272000, input: ['text', 'image'] })
  adapter.config = {
    resolveApiKey: async () => 'offline-fixture',
    resolveAttachments: () => ({ readImageRequest: async () => ({
      width: 1600, height: 1536, bytes: 1048576, mediaType: 'image/jpeg', data: new Uint8Array(1048576),
    }) }),
  }
  const ref = { attachmentId: `sha256:${'c'.repeat(64)}`, width: 1600, height: 1536, bytes: 1048576, mediaType: 'image/png' }
  const page = (id, count) => ({ id, role: 'tool', toolCallId: id,
    content: Array.from({ length: count }, () => ({ type: 'image', attachment: ref })) })
  let failure
  try {
    for await (const _chunk of adapter.streamWithSnapshot({ provider: 'hi', model: 'gpt-6-luna',
      messages: [page('new', 20)], maxTokens: 16384 }, snapshot)) assert.fail('must refuse before dispatch')
  } catch (error) { failure = error }
  assert.equal(providerCalls, 0)
  assert.notEqual(failure?.code, 'IMAGE_OFFLOAD_REQUIRED', 'byte budget must not request removal of six unobserved images')
  assert.match(failure?.message ?? '', /unobserved/)
  for (const [oldCount, expected] of [[8, 'IMAGE_OFFLOAD_REQUIRED'], [3, 'UNOBSERVED_IMAGE_BUDGET']]) {
    failure = undefined
    try {
      for await (const _chunk of adapter.streamWithSnapshot({ provider: 'hi', model: 'gpt-6-luna', messages: [
        page('old', oldCount), { id: 'observed', role: 'assistant', content: [{ type: 'text', text: 'old batch checked' }] },
        page('new', 20 - oldCount),
      ], maxTokens: 16384 }, snapshot)) assert.fail('must refuse before dispatch')
    } catch (error) { failure = error }
    assert.equal(failure?.code, expected)
    if (expected === 'IMAGE_OFFLOAD_REQUIRED') assert.equal(failure.failure.offloadImages, 6)
    assert.equal(providerCalls, 0)
  }
})

test('image pressure prices request pixels and only offloads observed history', async () => {
  const { imageRequestPricing, boundImageRequest, preserveImageObservations, imageOverflowOffload } = await contextModule()
  const ref = { attachmentId: `sha256:${'a'.repeat(64)}`, mediaType: 'image/png', width: 1600, height: 1536, bytes: 12345 }
  const helpers = {
    dimensions: (width, height) => ({ width, height }),
    handle: ref => `read ${ref.attachmentId}`,
    omitted: ref => `omitted; read ${ref.attachmentId}`,
  }
  const pricing = imageRequestPricing(helpers)
  assert.equal(pricing.priceImages([{ type: 'image', attachment: ref }])[0].visualTokens, 4800)
  assert.deepEqual(pricing.priceImages([{ type: 'image', attachment: ref, offloaded: true }])[0], { visualTokens: 0, text: helpers.omitted(ref) })
  const page = (id, attachment, count) => ({ id, role: 'tool', toolCallId: id, content: [
    { type: 'text', text: JSON.stringify({ build_id: 'r8', evidence_id: id, reader: 'read_component_first_use_images' }) },
    ...Array.from({ length: count }, () => ({ type: 'image', attachment })),
  ] })
  const observe = (id, text) => ({ id, role: 'assistant', content: [{ type: 'text', text }] })
  const messages = [page('page1', ref, 20), observe('observation1', 'page1: CAD identity checked; gap remains unverified'),
    page('page2', { ...ref, attachmentId: `sha256:${'b'.repeat(64)}` }, 20)]
  const request = { provider: 'hi', model: 'gpt-6-luna', messages, maxTokens: 128000 }
  const result = boundImageRequest(request, 180000, pricing)
  assert.ok(result.offloadImages > 0 && result.offloadImages <= 20)
  assert.equal(result.maxTokens, 16384)
  assert.equal(boundImageRequest({ ...request, messages: [messages[2]] }, 180000, pricing).offloadImages, 0)
  assert.equal(imageOverflowOffload(request, 'CONTEXT_WINDOW_EXCEEDED'), 10)
  assert.equal(imageOverflowOffload({ ...request, messages: [messages[2]] }, 'CONTEXT_WINDOW_EXCEEDED'), 0)
  assert.equal(imageOverflowOffload(request, 'SERVER'), 0)
  assert.throws(() => boundImageRequest({ ...request, messages: [messages[2]] }, 50000, pricing), /unobserved/)
  const summary = preserveImageObservations([{ type: 'text', text: 'concise checkpoint' }], messages.slice(0, 2))
  const checkpoint = { id: 'checkpoint', role: 'user', source: { kind: 'compact-checkpoint' }, content: summary }
  const again = preserveImageObservations([{ type: 'text', text: 'next checkpoint' }], [checkpoint])
  assert.deepEqual(again[1], summary[1])
  assert.ok(again[1].text.includes(ref.attachmentId))
  assert.ok(again[1].text.includes('observation1'))
  assert.ok(again[1].text.includes('gap remains unverified'))
  assert.ok(again[1].text.includes('read_component_first_use_images'))
  assert.equal(messages[0].content[1].offloaded, undefined)
  const severalViews = [page('top', ref, 1), page('perspective', ref, 1), observe('views', 'both existing views checked')]
  const views = preserveImageObservations([], severalViews)
  const records = JSON.parse(views[0].text.split('\n').slice(1).join('\n'))
  assert.equal(records.filter(record => record.observations.length > 0).length, 2)
  assert.throws(() => preserveImageObservations([], [page('large', ref, 513)]), /bound/)
})

test('installed DSH repairs replay the real image trace without model calls or writes', {
  skip: !process.env.DSH_CONTEXT_HOST_ENTRY || !process.env.DSH_CONTEXT_TRACE,
}, async () => {
  const { execFileSync } = await import('node:child_process')
  const { createRequire } = await import('node:module')
  const { pathToFileURL } = await import('node:url')
  const { createHash } = await import('node:crypto')
  const { readFileSync } = await import('node:fs')
  const { boundImageRequest } = await contextModule()
  const require = createRequire(process.env.DSH_CONTEXT_HOST_ENTRY)
  const { TokenMeter } = await import(pathToFileURL(require.resolve('@deepseek-ai/dsh-token-meter')).href)
  const { PiAiAdapter } = await hostModule('@deepseek-ai/dsh-llm-pi-ai')
  const { default: BasicCompaction } = await hostModule('@deepseek-ai/dsh-compaction-basic')
  const { deriveEventMessage } = await import(pathToFileURL(require.resolve('@deepseek-ai/dsh-session')).href)
  const { apply: imageOffload } = await import(pathToFileURL(require.resolve('@deepseek-ai/dsh-compaction-image-offload')).href)
  const digest = () => createHash('sha256').update(readFileSync(process.env.DSH_CONTEXT_TRACE)).digest('hex')
  const before = digest()
  const raw = execFileSync('zstd', ['-dc', '--', process.env.DSH_CONTEXT_TRACE], { maxBuffer: 64 * 1024 * 1024 }).toString()
  const events = raw.trim().split('\n').map(JSON.parse).filter(event => Number.isInteger(event.seq))
  const adapter = Object.create(PiAiAdapter.prototype)
  adapter.config = { resolveApiKey() { throw new Error('NO_MODEL_CALLS') } }
  adapter.current = () => ({ profiles: new Map([['hi', { requestImagePixelBudget: 4194304 }]]) })
  adapter.modelOf = () => ({ contextWindow: 272000 })
  assert.equal(adapter.zfContextRepairVersion(), 2)
  assert.equal(adapter.imageRequestPricing('other', 'gpt-6-luna'), undefined)
  const pricing = adapter.imageRequestPricing('hi', 'gpt-6-luna')
  const meter = Object.create(TokenMeter.prototype)
  Object.defineProperties(meter, {
    states: { value: new WeakMap() },
    ctx: { value: { get: name => name === 'llm' ? { imageRequestPricing: () => pricing } : undefined } },
  })
  const projected = new Map()
  let projection
  const hooks = new Map()
  const session = {
    id: 'offline', seq: 391,
    eventAt: seq => events.find(event => event.seq === seq),
    deriveEventMessage: event => projected.get(event.seq) ?? deriveEventMessage(event),
    append(type, data) {
      assert.equal(type, 'image/offload')
      const replacements = projection.project({ type, data }, { nodes: this.surface.nodes, events, baseSeq: 0, messages: projected })
      for (const [seq, message] of replacements) projected.set(seq, message)
    },
    requestHeader: () => events.filter(event => event.type === 'request/header').at(-1).data.header,
    toolHistory: () => [],
  }
  const measurement = meter.measure(session)
  const state = meter.states.get(session)
  session.surface = { nodes: measurement.nodes.map(node => node.seq), replaceGeneration: 0 }
  const messages = () => session.surface.nodes.map(seq => session.deriveEventMessage(session.eventAt(seq))).filter(Boolean)
  const header = session.requestHeader()
  const request = { ...header.config, tools: header.tools, messages: messages() }
  assert.equal(state.surface.reduce((count, node) => count + node.images.length, 0), 81)
  const imagePrice = pricing.priceImages(state.surface.flatMap(node => node.images)).reduce((count, price) => count + price.visualTokens, 0)
  assert.ok(imagePrice > 300000, '81 real request images cannot cost only their 4131 reference tokens')
  const selector = Object.create(BasicCompaction.prototype)
  selector.config = { auto: true, thresholdRatio: .8, retainRatio: .16, headroomTokens: 65536, maxTokens: 65536,
    compactionRetries: 1, modelPolicies: [], summarizationProvider: '', summarizationModel: '' }
  Object.defineProperty(selector, 'ctx', { value: { tokenMeter: meter, get() {}, llm: {
    resolveModelInfo: async () => ({ context: { contextWindow: 272000 }, defaultMaxTokens: 16384 }),
  } } })
  let selected
  const selectedOnly = new Error('OFFLINE_SELECTION_ONLY')
  selector.compactRegion = async (start, end) => { selected = { start, end }; throw selectedOnly }
  await assert.rejects(selector.compactIfNeeded({ session }, 'pressure', new AbortController().signal), error => error === selectedOnly)
  const start = state.surface.findIndex(node => node.seq === selected.start)
  const end = state.surface.findIndex(node => node.seq === selected.end)
  assert.ok(state.surface.slice(start, end + 1).some(node => node.images.length > 0), 'automatic compaction must select image-bearing history, not only old summaries')
  const budget = boundImageRequest(request, 272000, pricing)
  assert.ok(budget.offloadImages > 0)
  let failure
  try {
    for await (const _chunk of adapter.streamWithSnapshot(request, adapter.current())) assert.fail('must refuse before any provider call')
  } catch (error) { failure = error }
  assert.equal(failure?.code, 'IMAGE_OFFLOAD_REQUIRED')
  assert.equal(failure.failure.offloadImages, budget.offloadImages)
  imageOffload({ sessions: { registerMessageProjection: value => { projection = value } }, on: (name, listener) => hooks.set(name, listener) })
  const recovered = hooks.get('agent/request-error')({ agent: { session }, failure: failure.failure }, () => assert.fail('must recover'))
  assert.deepEqual(await recovered, { kind: 'retry' })
  const after = boundImageRequest({ ...request, messages: messages() }, 272000, pricing)
  assert.equal(after.offloadImages, 0)
  assert.ok(after.inputTokens + after.maxTokens + 16384 <= 272000)
  const compaction = Object.create(BasicCompaction.prototype)
  compaction.config = { thresholdRatio: .8, retainRatio: .16, headroomTokens: 65536, maxTokens: 65536,
    modelPolicies: [], summarizationProvider: '', summarizationModel: '' }
  let captured
  Object.defineProperty(compaction, 'ctx', { value: { llm: { async *stream(options) {
    captured = options
    const bound = boundImageRequest(options, 272000, pricing)
    assert.equal(bound.offloadImages, 0, 'summary request also fits without reattaching historical images')
    yield { type: 'block-start', index: 0, blockType: 'text' }
    yield { type: 'block-end', index: 0, block: { type: 'text', text: 'offline checkpoint; no provider invoked' } }
    yield { type: 'finish', reason: { kind: 'stop' } }
  } } } })
  const summary = await compaction.summarize({ messages: messages(), tools: header.tools }, { session, options: {} })
  assert.equal(captured.provider, 'hi')
  assert.equal(captured.model, 'gpt-6-luna')
  assert.equal(captured.maxTokens, 8192)
  assert.equal(summary.maxTokens, 8192)
  const ledger = JSON.parse(summary.summary.at(-1).text.split('\n').slice(1).join('\n'))
  assert.equal(ledger.reduce((count, entry) => count + entry.images.length, 0), 81)
  assert.equal(ledger.filter(entry => entry.observations.length > 0).length, 7)
  for (const entry of ledger) {
    assert.equal(typeof entry.reader?.name, 'string', 'image observations retain the exact source reader invocation')
    const original = messages().find(message => message.id === entry.messageId)
    assert.deepEqual(entry.images, original.content.filter(block => block.type === 'image').map(block => block.attachment))
    for (const observation of entry.observations) {
      const source = messages().find(message => message.id === observation.messageId)
      assert.equal(observation.text, source.content.filter(block => block.type === 'text').map(block => block.text).join('\n'))
    }
  }
  assert.equal(digest(), before, 'original trace is immutable')
})

/** A host context that records listeners so a test can fire host events. */
function host() {
  const listeners = new Map()
  const sent = []
  const agents = new Map()
  const sessions = new Map()
  const services = new Map()
  const ctx = {
    on: (name, listener, options) => listeners.set(name, { listener, options }),
    get: name => (name === 'agents' ? agents : name === 'sessions' ? sessions : services.get(name)),
  }
  const turns = relayEvents(ctx, frame => sent.push(frame))
  const stream = (session, frame) => listeners.get('agent/assistant-stream').listener({ agent: { session: { header: { id: session } } }, frame })
  const event = (session, type, data) => listeners.get('session/event').listener({ header: { id: session } }, { type, data })
  const claim = (session, id) => listeners.get('agent/inbox/claimed').listener({ agent: { session: { header: { id: session } } }, message: { id } })
  return { listeners, sent, stream, event, agents, sessions, services, turns, claim }
}

test('host ACP tags delayed outputs and publishes turn settlement only after the output tail', {
  skip: !process.env.DSH_CONTEXT_HOST_ENTRY || process.env.DSH_CONTEXT_SOURCE !== '1',
}, async () => {
  const { ZfTestAcpSession } = await hostModule('@deepseek-ai/dsh-acp')
  const order = []
  let release
  const gate = new Promise(resolve => { release = resolve })
  const session = { id: 'parent', requestContext: () => undefined }
  const ctx = { get() {}, emit: (method, value) => order.push({ method, value }), logger: { warn() {} } }
  const record = new ZfTestAcpSession(ctx, { agent: { session } }, { releaseTurn() {} }, async value => {
    await gate
    if (value._meta.zfHostTurn === 6) throw new Error('PRIVATE_DELIVERY_SECRET')
    order.push({ method: 'update', value })
  })
  record.onSessionEvent(session, { seq: 1, type: 'assistant/message', data: {
    turn: 4, message: { id: 'm4', content: [{ type: 'text', text: 'independent work' }] },
  } })
  record.onSessionEvent(session, { seq: 2, type: 'turn/end', data: { turn: 4, reason: { kind: 'completed' } } })
  record.onSessionEvent(session, { seq: 3, type: 'tool/call', data: { turn: 5, callId: 'c5', name: 'read_fact_view', arguments: '{}' } })
  record.onSessionEvent(session, { seq: 4, type: 'turn/end', data: { turn: 5, reason: {
    kind: 'error', error: { code: 'PI_AI_ERROR', message: 'PRIVATE_PROVIDER_SECRET' },
  } } })
  record.onSessionEvent(session, { seq: 5, type: 'tool/call', data: { turn: 6, callId: 'c6', name: 'read', arguments: '{}' } })
  record.onSessionEvent(session, { seq: 6, type: 'turn/end', data: { turn: 6, reason: { kind: 'completed' } } })
  record.onSessionEvent(session, { seq: 7, type: 'turn/end', data: { turn: 7, reason: {
    kind: 'error', error: { code: 'PRIVATE_UNKNOWN_CODE', message: 'PRIVATE_PROVIDER_SECRET' },
  } } })
  record.onSessionEvent(session, { seq: 8, type: 'turn/end', data: { turn: 8, reason: { kind: 'blocked' } } })
  await Promise.resolve()
  assert.equal(order.length, 0, 'raw turn/end is not an output-drained completion')
  release()
  await record.drainUpdates()
  assert.deepEqual(order.map(item => [item.method, item.value._meta?.zfHostTurn ?? item.value.turn]), [
    ['update', 4], ['zf-tui/acp-turn-settled', 4], ['update', 5], ['zf-tui/acp-turn-settled', 5],
    ['zf-tui/acp-turn-settled', 6], ['zf-tui/acp-turn-settled', 7], ['zf-tui/acp-turn-settled', 8],
  ])
  assert.deepEqual(order.filter(item => item.method === 'zf-tui/acp-turn-settled').map(item => item.value), [
    { sessionId: 'parent', turn: 4, state: 'completed' },
    { sessionId: 'parent', turn: 5, state: 'failed', failureCategory: 'provider', failureCode: 'PI_AI_ERROR' },
    { sessionId: 'parent', turn: 6, state: 'failed', failureCategory: 'output_delivery' },
    { sessionId: 'parent', turn: 7, state: 'failed', failureCategory: 'provider' },
    { sessionId: 'parent', turn: 8, state: 'failed', failureCategory: 'unknown' },
  ])
  assert.ok(!JSON.stringify(order).includes('PRIVATE_'))
})

test('background lifecycle is attributed by run and idle steering wakes the parent exactly once', async () => {
  const { listeners, sent, event, agents, sessions, turns, claim } = host()
  sessions.set('child', { header: { id: 'child', parentSession: 'parent' } })
  const start = listeners.get('subagent/start').listener
  const end = listeners.get('subagent/end').listener
  start({ id: 'child', runId: 'run-1' })
  start({ id: 'child', runId: 'run-1' })
  start({ id: 'unknown', runId: 'forged' })
  let wakes = 0
  agents.set('parent', { status: 'idle', followup(message) {
    wakes += 1
    event('parent', 'turn/start', { turn: 2 })
    claim('parent', message.id)
  } })
  turns.steer('parent', 'continue independent work', value => ({ ...value, id: 'steer-1' }))
  assert.equal(wakes, 1)
  assert.equal(sent.filter(frame => frame.params.event.type === 'steer').length, 1)
  end({ id: 'different', runId: 'run-1' })
  end({ id: 'child', runId: 'run-1' })
  end({ id: 'child', runId: 'run-1' })
  assert.deepEqual(sent.filter(frame => frame.params.event.type === 'subagent-run').map(frame =>
    [frame.params.sessionId, frame.params.event.runId, frame.params.event.state]), [
    ['parent', 'run-1', 'start'], ['parent', 'run-1', 'end'],
  ])
  assert.equal(wakes, 1, 'the plugin never resends the settlement as another prompt')
  const settled = listeners.get('zf-tui/acp-turn-settled').listener
  for (const [turn, category, code, expected] of [
    [2, 'provider', 'RATE_LIMIT', { failureCategory: 'provider', failureCode: 'RATE_LIMIT' }],
    [3, 'output_delivery', 'PI_AI_ERROR', { failureCategory: 'output_delivery' }],
    [4, 'PRIVATE_CATEGORY', 'PRIVATE_CODE', { failureCategory: 'unknown' }],
    [5, 'provider', 'PRIVATE_CODE', { failureCategory: 'provider' }],
  ]) {
    settled({ sessionId: 'parent', turn, state: 'failed', failureCategory: category, failureCode: code, message: 'PRIVATE_MESSAGE' })
    assert.deepEqual(sent.at(-1).params.event, { type: 'host-settled', turn, state: 'failed', ...expected })
  }
  assert.ok(!JSON.stringify(sent).includes('PRIVATE_'))
})

test('background cancellation waits for exact descendants and parent idleness without a new prompt', async () => {
  const { listeners, agents, sessions, services, turns } = host()
  sessions.set('child', { header: { parentSession: 'parent' } })
  listeners.get('subagent/start').listener({ id: 'child', runId: 'run-1' })
  let release, cancels = 0, idle = 0
  const gate = new Promise(resolve => { release = resolve })
  const agent = { status: 'idle', cancel() { cancels += 1 }, async whenIdle() { idle += 1 } }
  agents.set('parent', agent)
  services.set('subagents', { async drainContinuableChildren(parent, ids) {
    assert.equal(parent, agent)
    assert.deepEqual(ids, ['child'])
    await gate
    listeners.get('subagent/end').listener({ id: 'child', runId: 'run-1' })
  } })
  const stopping = turns.cancelBackground('parent')
  await Promise.resolve()
  assert.equal(cancels, 0)
  release()
  assert.deepEqual(await stopping, { stopped: true })
  assert.equal(cancels, 1)
  assert.equal(idle, 1)
  services.set('subagents', { async drainContinuableChildren() { throw new Error('teardown failed') } })
  await assert.rejects(turns.cancelBackground('parent'), /teardown failed/)
})
const chunk = type => ({ type: 'chunk', chunk: { type, text: 'PRIVATE model output' } })

test('durable steps distinguish model calls and retries keep the same step identity', () => {
  const { sent, stream, event } = host()
  event('s', 'turn/start', { turn: 3 })
  event('s', 'step/start', { turn: 3, step: 1 })
  stream('s', { type: 'start' })
  stream('s', chunk('text-delta'))
  event('s', 'step/start', { turn: 3, step: 1 })
  event('s', 'step/end', { turn: 2, step: 1 })
  event('s', 'step/end', { turn: 3, step: 99 })
  event('s', 'step/end', { turn: 3, step: 1 })
  event('s', 'step/start', { turn: 3, step: 2 })
  stream('s', { type: 'start' })
  event('s', 'step/start', { turn: 3, step: 1 })
  event('s', 'step/end', { turn: 3, step: 2 })
  event('s', 'turn/end', { turn: 3 })
  event('s', 'step/start', { turn: 3, step: 3 })
  const steps = sent.filter(frame => frame.params.event.type === 'step').map(frame => frame.params.event)
  assert.deepEqual(steps, [
    { type: 'step', turn: 3, step: 1, state: 'start' }, { type: 'step', turn: 3, step: 1, state: 'end' },
    { type: 'step', turn: 3, step: 2, state: 'start' }, { type: 'step', turn: 3, step: 2, state: 'end' },
  ])
  assert.deepEqual(sent.filter(frame => frame.params.event.type === 'stage').map(frame => frame.params.event.step), [1, 1, 2])
})

test('a stage is announced once per change and never carries model output', () => {
  const { listeners, sent, stream, event } = host()
  assert.equal(listeners.get('agent/assistant-stream').options.global, true)
  stream('s', { type: 'start', turn: 't', step: 1 })
  for (const type of ['reasoning-delta', 'reasoning-delta', 'usage', 'text-delta', 'text-delta', 'tool-call-delta']) stream('s', chunk(type))
  stream('other', chunk('text-delta'))
  // The next request of the same turn waits again; a new turn starts clean.
  stream('s', { type: 'start' })
  event('s', 'turn/end', {})
  stream('s', { type: 'start' })
  const stages = sent.filter(frame => frame.params.event.type === 'stage')
  assert.deepEqual(stages.map(frame => [frame.method, frame.params.sessionId, frame.params.event.stage]), [
    [EVENT_METHOD, 's', 'waiting'], [EVENT_METHOD, 's', 'thinking'], [EVENT_METHOD, 's', 'response'],
    [EVENT_METHOD, 'other', 'response'], [EVENT_METHOD, 's', 'waiting'], [EVENT_METHOD, 's', 'waiting'],
  ])
  // A stage is only a label: the generated text travels in `delta` events alone.
  assert.ok(!JSON.stringify(stages).includes('PRIVATE'))
})

test('generated text is batched per block and always closed by an end marker', async () => {
  const { sent, stream } = host()
  const delta = (type, index, text) => ({ type: 'chunk', chunk: { type, index, text } })
  stream('s', { type: 'start' })
  stream('s', delta('reasoning-delta', 0, 'weigh '))
  stream('s', delta('reasoning-delta', 0, 'options'))
  stream('s', delta('text-delta', 1, 'Hel'))
  stream('s', delta('text-delta', 1, 'lo'))
  stream('s', delta('usage', 0, 'ignored'))
  stream('s', { type: 'end', outcome: { kind: 'committed', eventType: 'assistant/message', seq: 9 } })
  const events = sent.map(frame => frame.params.event)
  assert.deepEqual(events, [
    { type: 'stage', stage: 'waiting' },
    { type: 'stage', stage: 'thinking' },
    { type: 'delta', kind: 'thinking', block: 0, text: 'weigh options' },
    { type: 'stage', stage: 'response' },
    { type: 'delta', kind: 'text', block: 1, text: 'Hello' },
    { type: 'delta-end', committed: true },
  ])
  // A batch left alone is sent by its timer; an abandoned attempt is closed as such.
  stream('s', { type: 'start' })
  stream('s', delta('text-delta', 0, 'partial'))
  await new Promise(resolve => setTimeout(resolve, 120))
  assert.deepEqual(sent.at(-1).params.event, { type: 'delta', kind: 'text', block: 0, text: 'partial' })
  stream('s', { type: 'end', outcome: { kind: 'abandoned' } })
  assert.deepEqual(sent.at(-1).params.event, { type: 'delta-end', committed: false })
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
  // Host turn boundaries are relayed too; this test is about steering receipts.
  const steers = () => sent.map(frame => frame.params.event).filter(event => event.type === 'steer')
  assert.deepEqual(steers(), [{ type: 'steer', id: 'message-1', state: 'delivered' }])
  // A message the turn never claimed is not reported as delivered afterwards.
  turns.steer('s', 'go right', create)
  event('s', 'turn/end', {})
  claim('s', 'message-2')
  assert.equal(steers().length, 1)
  assert.throws(() => turns.steer('s', 'late', create), /no turn is running/)
  // A host refusal leaves nothing pending.
  event('s', 'turn/start', {})
  agent.steer = () => { throw new Error('host refused') }
  assert.throws(() => turns.steer('s', 'again', create), /host refused/)
  claim('s', 'message-3')
  assert.equal(steers().length, 1)
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

test('host turns, goal changes and goal rounds are relayed without the objective', () => {
  const { sent, event } = host()
  event('s', 'turn/start', {})
  event('s', 'goal/change', { kind: 'goal/change', operation: 'create', goal: { objective: 'PRIVATE objective' } })
  event('s', 'user/message', { content: [{ type: 'text', text: '<goal_round>\nObjective: "PRIVATE"\nRound: 2/30\n' }] })
  event('s', 'user/message', { content: [{ type: 'text', text: 'Round: 9/9 typed by a person' }] })
  event('s', 'goal/change', { operation: 'block' })
  event('s', 'turn/end', {})
  assert.deepEqual(sent.map(frame => frame.params.event), [
    { type: 'host-turn', state: 'start' },
    { type: 'goal', operation: 'create' },
    { type: 'goal-round', round: 2, max: 30 },
    { type: 'goal', operation: 'block' },
    { type: 'host-turn', state: 'end' },
  ])
  assert.ok(!JSON.stringify(sent).includes('PRIVATE'))
})

test('a modeling session forbids dsh goals: a created goal is cleared before any round', async () => {
  const listeners = new Map()
  const sent = []
  const agent = { id: 'agent-s' }
  let goal
  const cleared = []
  const goals = {
    get: owner => (owner === agent ? goal : undefined),
    clear: (owner, ref) => { cleared.push(ref); goal = undefined },
  }
  const ctx = {
    on: (name, listener) => listeners.set(name, { listener }),
    get: name => (name === 'agents' ? new Map([['s', agent]]) : name === 'goals' ? goals : undefined),
    logger: { warn() {}, info() {} },
  }
  const turns = relayEvents(ctx, frame => sent.push(frame))
  const event = (session, type, data) => listeners.get('session/event').listener({ header: { id: session } }, { type, data })
  // An existing goal is cleared at once when the owner forbids goals.
  goal = { id: 'g1', revision: 1 }
  assert.deepEqual(turns.setGoals('s', false), { allowed: false })
  assert.deepEqual(cleared, [{ id: 'g1', revision: 1 }])
  // A goal the agent creates later is cleared after the host's dispatch.
  goal = { id: 'g2', revision: 1 }
  event('s', 'goal/change', { operation: 'create' })
  assert.equal(cleared.length, 1, 'never re-enters the host dispatch')
  await new Promise(resolve => setTimeout(resolve, 5))
  assert.deepEqual(cleared[1], { id: 'g2', revision: 1 })
  assert.ok(sent.some(frame => frame.params.event.operation === 'suppressed'))
  // Other sessions keep their goals.
  turns.setGoals('s', true)
  goal = { id: 'g3', revision: 1 }
  event('s', 'goal/change', { operation: 'create' })
  await new Promise(resolve => setTimeout(resolve, 5))
  assert.equal(cleared.length, 2)
})

test('MCP config activation extends only the modeling domain timeout for new and resumed clients', () => {
  const listeners = new Map()
  const disposers = []
  const ctx = {
    on: (name, listener) => listeners.set(name, listener),
    get: name => name === 'appReady' ? { onReady() {} } : name === 'appExit' ? () => {} : undefined,
    effect: callback => disposers.push(callback()),
    plugin() {},
    logger: { warn() {}, info() {} },
  }
  try {
    apply(ctx)
    const resolve = listeners.get('internal/config')
    assert.equal(typeof resolve, 'function')
    for (const lifecycle of ['new', 'resume']) {
      const raw = { transport: 'stdio', serverName: 'modeling_domain', toolCallTimeoutMs: 60_000 }
      let continued = false
      const result = resolve.call({ runtime: { name: 'mcp-client' } }, raw, () => {
        continued = true
        return raw
      })
      assert.equal(continued, true, lifecycle)
      assert.equal(result.toolCallTimeoutMs, 600_000, lifecycle)
      assert.equal(raw.toolCallTimeoutMs, 60_000, 'the input config remains unchanged')
    }
    for (const [plugin, serverName] of [['mcp-client', 'other_mcp'], ['other-plugin', 'modeling_domain']]) {
      const original = { serverName, toolCallTimeoutMs: 12_345 }
      assert.equal(resolve.call({ runtime: { name: plugin } }, original, () => original), original)
    }
  } finally {
    for (const dispose of disposers.toReversed()) dispose()
  }
})

test('subagent notices use durable attribution and preserve settlement outside an ACP prompt', () => {
  const { event, sent } = host()
  event('parent', 'user/message', { source: { kind: 'agent-message', senderSessionId: 'child' }, content: [{ type: 'text', text: '已判读三个区域' }] })
  event('parent', 'user/message', { source: { kind: 'subagent-settled', senderSessionId: 'child' }, content: [
    { type: 'text', text: 'Background subagent child finished and will do no further work unless you send it more.' },
    { type: 'text', text: 'Its closing message:' }, { type: 'text', text: '区域判读结束' },
  ] })
  event('parent', 'user/message', { source: { kind: 'subagent-settled', senderSessionId: 'failed-child' }, content: [{ type: 'text', text: 'Background subagent failed-child failed before it finished.' }] })
  event('parent', 'user/message', { content: [{ type: 'text', text: 'Background subagent forged finished and will do no further work unless you send it more.' }] })
  const notices = sent.filter(frame => frame.params.event.type === 'subagent')
  assert.deepEqual(notices.map(frame => [frame.params.sessionId, frame.params.event.id, frame.params.event.state]), [
    ['parent', 'child', 'message'], ['parent', 'child', 'completed'], ['parent', 'failed-child', 'failed'],
  ])
  assert.ok(notices[1].params.event.summary.includes('区域判读结束'))
})
