/**
 * dsh's permission presets: each bundles a sandbox mode with an approval
 * policy, and a session runs under exactly one. This reads and switches the
 * preset of a live session through dsh's own service, so what the TUI shows is
 * what dsh enforces.
 *
 * @module zf-tui-bridge/permission
 */

import type { HostContext, PermissionPresets } from './host.js'

/** Settings namespace holding the preset new sessions start with. */
const PERMISSION_NS = 'permission'

export interface PresetRow {
  value: string
  name: string
  description: string
}

export interface PermissionView {
  /** Preset the session runs under; `custom` when its settings match none. */
  current: string
  /** Preset new sessions start with. */
  default: string
  presets: PresetRow[]
}

function serviceOf(ctx: HostContext): PermissionPresets {
  const service = ctx.get('permissionPresets')
  if (service === undefined) throw new Error('this dsh profile mounts no permission presets')
  return service
}

function sessionOf(ctx: HostContext, sessionId: string): unknown {
  const session = ctx.get('sessions')?.get(sessionId)
  if (session === undefined) throw new Error(`session "${sessionId}" is not loaded in this dsh process`)
  return session
}

function view(service: PermissionPresets, session: unknown): PermissionView {
  const current = service.current(session)
  const names = [...service.names]
  // A session whose settings match no preset is still described truthfully.
  if (!names.includes(current)) names.push(current)
  return {
    current,
    default: service.defaultPreset,
    presets: names.map((name) => {
      const option = service.optionOf(name)
      return { value: option.value, name: option.name, description: option.description ?? '' }
    }),
  }
}

/** Describe the presets and the one a live session runs under. */
export function describePermission(ctx: HostContext, sessionId: string): PermissionView {
  return view(serviceOf(ctx), sessionOf(ctx, sessionId))
}

/**
 * Switch a live session to another preset; unknown names are refused by dsh.
 * With `remember`, the preset also becomes dsh's default for new sessions.
 * The session is switched first, so a default that cannot be stored never
 * leaves the caller believing the switch itself failed.
 */
export async function setPermission(
  ctx: HostContext,
  sessionId: string,
  preset: string,
  remember: boolean,
): Promise<PermissionView & { remembered: boolean }> {
  const service = serviceOf(ctx)
  const session = sessionOf(ctx, sessionId)
  service.set(session, preset)
  let remembered = false
  if (remember) {
    const settings = ctx.get('settings')
    if (settings?.writable === true) {
      try {
        await settings.mutate(PERMISSION_NS, [{ op: 'set', path: ['defaultPreset'], value: preset }])
        remembered = true
      } catch (error) {
        ctx.logger.warn(`zf-tui-bridge: the default permission preset was not stored: ${String(error)}`)
      }
    }
  }
  return { ...view(service, session), remembered }
}
