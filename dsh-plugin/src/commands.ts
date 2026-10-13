/**
 * dsh's own slash commands, such as `/compact` and `/plan`, listed and run for
 * one live session. They are the host's commands: the TUI names them and shows
 * their result, and keeps no command logic of its own.
 *
 * @module zf-tui-bridge/commands
 */

import type { HostContext } from './host.js'

const MAX_COMMANDS = 64
const MAX_TEXT_CHARS = 2000

/**
 * Commands that start turns on their own. Their output would belong to no
 * prompt of this client and could not be shown, so they are refused here.
 */
const SELF_DRIVING = new Set(['goal'])

export interface CommandRow {
  name: string
  description: string
  /** What the command expects after its name, when it says so. */
  hint?: string
}

function parts(ctx: HostContext, sessionId: string) {
  const commands = ctx.get('commands')
  if (commands === undefined) throw new Error('this dsh profile mounts no command registry')
  const agent = ctx.get('agents')?.get(sessionId)
  if (agent === undefined) throw new Error(`session "${sessionId}" has no agent in this dsh process`)
  return { commands, agent }
}

/** The commands a session can run, without the ones this client cannot show. */
export function listCommands(ctx: HostContext, sessionId: string): { commands: CommandRow[] } {
  const { commands, agent } = parts(ctx, sessionId)
  const rows: CommandRow[] = []
  for (const descriptor of commands.list(agent)) {
    if (rows.length >= MAX_COMMANDS) break
    if (!/^[a-z][a-z0-9-]{0,63}$/.test(descriptor.name) || SELF_DRIVING.has(descriptor.name)) continue
    const hint = descriptor.input?.hint
    rows.push({
      name: descriptor.name,
      description: (descriptor.description ?? '').slice(0, 200),
      ...(typeof hint === 'string' && hint !== '' ? { hint: hint.slice(0, 80) } : {}),
    })
  }
  return { commands: rows }
}

/**
 * Run one command line, e.g. `/compact`, and return what the host reports.
 * Only an idle session is accepted: a command changes session state, and the
 * host's own commands refuse or misbehave while a turn is running.
 */
export async function runCommand(
  ctx: HostContext,
  sessionId: string,
  line: string,
): Promise<{ kind: string; text: string }> {
  const { commands, agent } = parts(ctx, sessionId)
  const name = /^\/([a-z][a-z0-9-]*)/.exec(line.trim())?.[1]
  if (name === undefined) throw new Error('a command line starts with /name')
  if (SELF_DRIVING.has(name)) throw new Error(`/${name} starts turns on its own, which this client cannot show yet`)
  // `/plan <message>` would also send the message as a prompt of its own.
  if (name === 'plan' && !/^\/plan(\s+off)?$/.test(line.trim())) {
    throw new Error('use /plan or /plan off, then send the task as usual')
  }
  if (agent.status !== 'idle') throw new Error('a turn is running; wait for it to finish')
  const execution = await commands.execute(agent, line.trim(), [], new AbortController().signal)
  if (execution === undefined) throw new Error(`dsh has no command /${name}`)
  const { kind, text } = execution.result
  return { kind: kind === 'success' ? 'success' : 'error', text: (text ?? '').slice(0, MAX_TEXT_CHARS) }
}
