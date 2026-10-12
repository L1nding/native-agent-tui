/**
 * Bridge between the native agent TUI (a separate Rust process) and the dsh
 * host this plugin runs in. The TUI launches `dsh --profile <profile>` and
 * speaks newline-delimited JSON-RPC 2.0 over the child's stdio; this plugin
 * answers from the host's own services, so sessions, titles, commands and
 * settings are dsh's and are not kept a second time by the TUI.
 *
 * @module zf-tui-bridge
 */

import { createInterface } from 'node:readline'

export const name = 'zf-tui-bridge'

/** Host services the bridge reads; a profile without them cannot serve the TUI. */
export const inject = ['sessionPersistence', 'sessions']

export const PROTOCOL_VERSION = 1

/**
 * Mount the stdio server.
 * @param {import('@deepseek-ai/cordis').Context} ctx - host context with the injected services.
 */
export function apply(ctx) {
  // Handlers run outside this plugin's injection scope; capture services now.
  const persistence = ctx.sessionPersistence

  /** Write one frame. Stdout carries protocol frames only. */
  const send = (frame) => {
    process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', ...frame })}\n`)
  }

  const methods = {
    initialize() {
      return {
        protocolVersion: PROTOCOL_VERSION,
        serverInfo: { name: 'zf-tui-bridge', version: '0.1.0-alpha.0' },
      }
    },

    /** Stored sessions as dsh's persistence service lists them. */
    async 'session/list'() {
      const listed = await persistence.list()
      return { sessions: listed }
    },
  }

  const input = createInterface({ input: process.stdin, crlfDelay: Infinity })
  input.on('line', (line) => {
    let message
    try {
      message = JSON.parse(line)
    } catch {
      return
    }
    if (message === null || typeof message !== 'object' || typeof message.method !== 'string') return
    const { id, method, params } = message
    if (id === undefined) return
    const handler = methods[method]
    if (handler === undefined) {
      send({ id, error: { code: -32601, message: `unknown method: ${method}` } })
      return
    }
    Promise.resolve()
      .then(() => handler(params ?? {}))
      .then(
        (result) => send({ id, result }),
        (error) => send({ id, error: { code: -32603, message: String(error?.message ?? error) } }),
      )
  })
  // The TUI owns this process: when it closes our input, the host exits.
  input.on('close', () => {
    process.exit(0)
  })
  ctx.on('dispose', () => input.close())
}
