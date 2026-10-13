import assert from 'node:assert/strict'
import { test } from 'node:test'
import { listCommands, runCommand } from '../lib/commands.js'

function host(status = 'idle') {
  const agent = { status }
  const ran = []
  const commands = {
    list: () => [
      { name: 'compact', description: 'Summarise earlier history' },
      { name: 'plan', description: 'x'.repeat(500), input: { hint: 'on|off' } },
      { name: 'goal', description: 'Drive rounds' },
      { name: 'Bad Name', description: 'ignored' },
    ],
    execute: async (target, line, attachments, signal) => {
      ran.push({ target, line, attachments, aborted: signal.aborted })
      if (line.startsWith('/compact')) return { result: { kind: 'success', text: 'Compacted 3 history items.' } }
      if (line.startsWith('/plan')) return { result: { kind: 'error', text: 'y'.repeat(5000) } }
      return undefined
    },
  }
  const ctx = { get: name => (name === 'commands' ? commands : name === 'agents' ? new Map([['s', agent]]) : undefined) }
  return { ctx, agent, ran }
}

test('commands are listed bounded, without the ones this client cannot show', () => {
  const { ctx } = host()
  const { commands } = listCommands(ctx, 's')
  assert.deepEqual(commands.map(command => command.name), ['compact', 'plan'])
  assert.equal(commands[1].description.length, 200)
  assert.equal(commands[1].hint, 'on|off')
  assert.throws(() => listCommands(ctx, 'missing'), /has no agent/)
  assert.throws(() => listCommands({ get: () => undefined }, 's'), /no command registry/)
})

test('a command runs only on an idle session and reports the host result', async () => {
  const { ctx, agent, ran } = host()
  assert.deepEqual(await runCommand(ctx, 's', ' /compact '), { kind: 'success', text: 'Compacted 3 history items.' })
  assert.deepEqual(ran[0], { target: agent, line: '/compact', attachments: [], aborted: false })
  const failed = await runCommand(ctx, 's', '/plan off')
  assert.equal(failed.kind, 'error')
  assert.equal(failed.text.length, 2000)
  await assert.rejects(runCommand(ctx, 's', '/nope'), /dsh has no command \/nope/)
  await assert.rejects(runCommand(ctx, 's', 'compact'), /starts with \/name/)
  await assert.rejects(runCommand(ctx, 's', '/goal ship it'), /starts turns on its own/)
  await assert.rejects(runCommand(ctx, 's', '/plan build a house'), /then send the task as usual/)
  agent.status = 'running'
  await assert.rejects(runCommand(ctx, 's', '/compact'), /a turn is running/)
  assert.equal(ran.length, 3)
})
