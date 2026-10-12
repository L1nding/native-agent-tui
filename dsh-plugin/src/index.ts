/**
 * Bridge between the native agent TUI (a separate Rust process) and the dsh
 * host this plugin runs in. The TUI launches `dsh --profile <profile>` and
 * speaks newline-delimited JSON-RPC 2.0 over the child's stdio.
 *
 * One connection carries two method sets:
 *
 * - Methods under `zf/` are answered here from the host's own services, so
 *   sessions, titles, commands and settings are dsh's and are not kept a
 *   second time by the TUI.
 * - Every other frame is Agent Client Protocol. It is handed to dsh's own ACP
 *   server, mounted by this plugin on an in-memory stream, so agent turns,
 *   permission prompts and model selection are dsh's maintained implementation.
 *
 * @module zf-tui-bridge
 */

import { createInterface } from 'node:readline'
import { loadHostAcp, type Frame, type FrameId, type FrameStream, type HostContext } from './host.js'
import { listSessions, readHistory } from './sessions.js'

export const name = 'zf-tui-bridge'

/** Host services the bridge reads directly. */
export const inject = ['sessionPersistence', 'sessions']

export const PROTOCOL_VERSION = 1
const VERSION = '0.1.0-alpha.0'
const OWN_PREFIX = 'zf/'

/** Route for the sessions the TUI creates; absent fields keep the host's default. */
export interface BridgeConfig {
  provider?: string
  model?: string
}

type Handler = (params: Record<string, unknown>) => unknown

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/**
 * Mount the stdio server.
 * @param ctx - host context with the injected services.
 * @param config - route for sessions the TUI creates.
 */
export function apply(ctx: HostContext, config: BridgeConfig = {}): void {
  const logger = ctx.logger

  /** Write one frame. Stdout carries protocol frames only. */
  const send = (frame: Frame): void => {
    process.stdout.write(`${JSON.stringify(frame)}\n`)
  }
  const reply = (id: FrameId, result: unknown): void => send({ jsonrpc: '2.0', id, result })
  const fail = (id: FrameId, code: number, message: string): void =>
    send({ jsonrpc: '2.0', id, error: { code, message } })

  // dsh's ACP server reads client frames from `toAcp` and writes its own to `fromAcp`.
  const toAcp = new TransformStream<Frame, Frame>()
  const fromAcp = new TransformStream<Frame, Frame>()
  const acpInput = toAcp.writable.getWriter()
  let acp: 'loading' | 'mounted' | 'unavailable' = 'loading'
  let acpFailure = ''

  void (async () => {
    const reader = fromAcp.readable.getReader()
    for (;;) {
      const { value, done } = await reader.read()
      if (done) return
      send(value)
    }
  })()

  loadHostAcp().then(
    (module) => {
      const stream: FrameStream = { readable: toAcp.readable, writable: fromAcp.writable }
      ctx.plugin(module, {
        stream,
        ...(config.provider === undefined ? {} : { provider: config.provider }),
        ...(config.model === undefined ? {} : { model: config.model }),
      })
      acp = 'mounted'
    },
    (error: unknown) => {
      acp = 'unavailable'
      acpFailure = describe(error)
      logger.warn(`${name}: dsh's ACP server could not be loaded: ${acpFailure}`)
    },
  )

  const methods: Record<string, Handler> = {
    /** What this bridge adds on top of ACP. */
    'zf/hello': () => ({
      protocolVersion: PROTOCOL_VERSION,
      serverInfo: { name, version: VERSION },
      acp,
      methods: Object.keys(methods),
    }),

    /** Root sessions with dsh's titles, newest first; `cwd` narrows to one workspace. */
    'zf/session/list': (params) => listSessions(ctx, typeof params['cwd'] === 'string' ? params['cwd'] : undefined),

    /** Skills every installed provider offers for one workspace; bodies are not loaded. */
    'zf/skills/list': async (params) => {
      const registry = ctx.get('skills')
      if (registry === undefined) throw new Error('this dsh profile mounts no skill registry')
      const cwd = typeof params['cwd'] === 'string' ? params['cwd'] : undefined
      const skills = await registry.list(cwd === undefined ? {} : { cwd })
      return {
        skills: skills.map((skill) => ({
          name: skill.name,
          description: skill.description ?? '',
          source: skill.source ?? 'custom',
          modelInvocable: skill.invocation?.modelInvocable === true,
          userInvocable: skill.invocation?.userInvocable === true,
          ...(skill.path === undefined ? {} : { path: skill.path }),
        })),
      }
    },

    /** User and assistant text of one session, for showing a resumed conversation. */
    'zf/session/history': (params) => {
      const sessionId = params['sessionId']
      if (typeof sessionId !== 'string' || sessionId === '') throw new Error('sessionId is required')
      const query = ctx.get('sessionQuery')
      if (query === undefined) throw new Error('this dsh profile mounts no session query service')
      return readHistory(query, sessionId)
    },
  }

  const handleOwn = (frame: Frame): void => {
    const { id, method } = frame
    if (id === undefined || method === undefined) return
    const handler = methods[method]
    if (handler === undefined) {
      fail(id, -32601, `unknown method: ${method}`)
      return
    }
    const params =
      frame.params !== null && typeof frame.params === 'object' ? (frame.params as Record<string, unknown>) : {}
    Promise.resolve()
      .then(() => handler(params))
      .then(
        (result) => reply(id, result),
        (error: unknown) => fail(id, -32603, describe(error)),
      )
  }

  const input = createInterface({ input: process.stdin, crlfDelay: Infinity })
  input.on('line', (line) => {
    let frame: Frame
    try {
      const parsed: unknown = JSON.parse(line)
      if (parsed === null || typeof parsed !== 'object') return
      frame = parsed as Frame
    } catch {
      return
    }
    if (frame.method?.startsWith(OWN_PREFIX) === true) {
      handleOwn(frame)
      return
    }
    if (acp === 'unavailable') {
      if (frame.id !== undefined && frame.method !== undefined) {
        fail(frame.id, -32603, `ACP is unavailable: ${acpFailure}`)
      }
      return
    }
    // Requests, notifications and the client's answers to permission requests.
    acpInput.write(frame).catch(() => {})
  })

  // The TUI owns this process. When it closes our input the ACP connection
  // closes, which drains its agents; then the launcher shuts the tree down.
  input.on('close', () => {
    acpInput.close().catch(() => {})
    const exit = ctx.get('appExit')
    const ready = ctx.get('appReady')
    if (exit !== undefined && ready !== undefined) ready.onReady(() => { exit(0) })
    else process.exit(0)
  })
  ctx.effect(() => () => input.close(), `${name}.stdin`)
}
