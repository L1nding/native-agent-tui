/**
 * The slice of the dsh host this plugin touches, declared structurally.
 *
 * The host's packages are not dependencies of this package: they are resolved
 * at run time from the `dsh` install that loaded the plugin, so there is one
 * Cordis instance and the versions always match the host. Everything here is
 * therefore a shape the host is expected to have, not an import, and optional
 * wherever a host version may not provide it.
 *
 * @module zf-tui-bridge/host
 */

import { realpathSync } from 'node:fs'
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

export type FrameId = string | number | null

/** One JSON-RPC 2.0 frame in either direction. */
export interface Frame {
  jsonrpc?: string
  id?: FrameId
  method?: string
  params?: unknown
  result?: unknown
  error?: { code: number; message: string; data?: unknown }
}

/** A message stream as the ACP SDK defines it: parsed frames, not bytes. */
export interface FrameStream {
  readable: ReadableStream<Frame>
  writable: WritableStream<Frame>
}

/** Header of a stored session, as dsh's persistence service reports it. */
export interface SessionHeader {
  id: string
  cwd?: string
  createdAt?: number
  delegationDepth?: number
  agentPreset?: string
}

/** One element of `sessionPersistence.list()`. */
export interface StoredSession {
  header: SessionHeader
  revision?: string
  sizeBytes?: number
}

/** `ctx.sessionPersistence`: the host's session store. */
export interface SessionPersistence {
  list(signal?: AbortSignal): Promise<readonly StoredSession[]>
}

export interface Logger {
  warn(message: string): void
  info(message: string): void
}

/** `ctx.appReady`: the launcher's readiness gate. */
export interface AppReady {
  onReady(callback: () => void): () => void
}

/** A plugin module as Cordis loads it. */
export interface PluginModule {
  name?: string
  inject?: readonly string[]
  apply(ctx: HostContext, config: unknown): void
}

/** The Cordis context, narrowed to what the bridge uses. */
export interface HostContext {
  sessionPersistence: SessionPersistence
  logger: Logger
  plugin(plugin: PluginModule, config?: unknown): unknown
  effect(callback: () => () => void, label?: string): unknown
  get(name: 'appExit'): ((code: number) => void) | undefined
  get(name: 'appReady'): AppReady | undefined
  get(name: string): unknown
}

/**
 * Load dsh's ACP server from the host's own install. It is resolved from the
 * running `dsh` entry, not from this package, so it shares the host's single
 * Cordis instance and always matches the host's version.
 */
export async function loadHostAcp(): Promise<PluginModule> {
  const entry = process.argv[1]
  if (entry === undefined) throw new Error('the dsh entry script is unknown')
  const fromHost = createRequire(realpathSync(entry))
  const app = fromHost.resolve('@deepseek-ai/dsh-acp-app/package.json')
  const acp = createRequire(app).resolve('@deepseek-ai/dsh-acp')
  return (await import(pathToFileURL(acp).href)) as PluginModule
}
