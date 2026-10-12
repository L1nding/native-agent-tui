/**
 * Model providers, read and written through dsh's settings service the way
 * dsh's own settings pages do. The namespace applies live, so a saved provider
 * is usable without restarting the host. An API key goes to dsh's credential
 * service and is never read back.
 *
 * @module zf-tui-bridge/providers
 */

import type { HostContext, SettingsProvider } from './host.js'

/** Settings namespace of dsh's OpenAI/Anthropic-compatible provider plugin. */
export const PROVIDERS_NS = 'llm-pi-ai'

export interface ProviderModel {
  id: string
  name?: string
  contextWindow?: number
}

export interface ProviderRow {
  name: string
  baseURL: string
  api: string
  apiKeyEnv?: string
  models: ProviderModel[]
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined
}

function text(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

function settingsOf(ctx: HostContext): SettingsProvider {
  const settings = ctx.get('settings')
  if (settings === undefined) throw new Error('this dsh profile mounts no settings provider')
  return settings
}

/** Providers as currently resolved: composition defaults with the user's edits on top. */
function resolved(settings: SettingsProvider): Record<string, unknown> {
  const descriptor = settings.describe({ redactSecrets: true }).find((entry) => String(entry.ns) === PROVIDERS_NS)
  if (descriptor === undefined) throw new Error(`this dsh profile has no "${PROVIDERS_NS}" settings; the provider plugin is not installed`)
  return record(record(descriptor.value)?.['providers']) ?? {}
}

/** List the configurable providers with their models. */
export function listProviders(ctx: HostContext): { providers: ProviderRow[]; writable: boolean } {
  const settings = settingsOf(ctx)
  const providers: ProviderRow[] = []
  for (const [name, value] of Object.entries(resolved(settings))) {
    const provider = record(value)
    if (provider === undefined) continue
    const models: ProviderModel[] = []
    for (const entry of Array.isArray(provider['models']) ? provider['models'] : []) {
      const model = record(entry)
      const id = text(model?.['id'])
      if (model === undefined || id === '') continue
      const contextWindow = model['contextWindow']
      models.push({
        id,
        ...(text(model['name']) === '' ? {} : { name: text(model['name']) }),
        ...(typeof contextWindow === 'number' ? { contextWindow } : {}),
      })
    }
    const apiKeyEnv = text(provider['apiKeyEnv'])
    providers.push({
      name,
      baseURL: text(provider['baseURL']),
      api: text(provider['api']),
      ...(apiKeyEnv === '' ? {} : { apiKeyEnv }),
      models,
    })
  }
  return { providers, writable: settings.writable === true }
}

export interface SaveRequest {
  provider: string
  baseURL: string
  api: string
  model: string
  modelName?: string
  contextWindow?: number
  /** Credential reference used when the provider has none yet. */
  credential: string
  /** Stored with dsh's credential service when present; never returned. */
  apiKey?: string
}

/**
 * Add a provider, or change one and add or change one of its models. Other
 * models and fields this request does not name are kept.
 */
export async function saveProvider(ctx: HostContext, request: SaveRequest): Promise<{
  label: string
  credential: string
  keyStored: boolean
}> {
  const settings = settingsOf(ctx)
  if (settings.writable !== true) throw new Error('dsh settings are read-only in this profile')
  const current = record(resolved(settings)[request.provider])
  const credential = text(current?.['apiKeyEnv']) || request.credential
  const models: Record<string, unknown>[] = []
  let found = false
  for (const entry of Array.isArray(current?.['models']) ? (current['models'] as unknown[]) : []) {
    const model = record(entry)
    if (model === undefined) continue
    if (model['id'] !== request.model) {
      models.push(model)
      continue
    }
    found = true
    models.push({
      ...model,
      ...(request.modelName === undefined ? {} : { name: request.modelName }),
      ...(request.contextWindow === undefined ? {} : { contextWindow: request.contextWindow }),
    })
  }
  if (!found) {
    models.push({
      id: request.model,
      name: request.modelName ?? request.model,
      ...(request.contextWindow === undefined ? {} : { contextWindow: request.contextWindow }),
    })
  }
  const at = (field: string): string[] => ['providers', request.provider, field]
  await settings.mutate(PROVIDERS_NS, [
    { op: 'set', path: at('api'), value: request.api },
    { op: 'set', path: at('baseURL'), value: request.baseURL },
    { op: 'set', path: at('apiKeyEnv'), value: credential },
    { op: 'set', path: at('models'), value: models },
  ])
  let keyStored = false
  if (request.apiKey !== undefined && request.apiKey !== '') {
    const credentials = ctx.get('credentials')
    if (credentials === undefined) throw new Error('the provider was saved, but this dsh profile mounts no credential service for its key')
    await credentials.set(credential, request.apiKey)
    keyStored = true
  }
  return { label: `${request.provider}/${request.model}`, credential, keyStored }
}

/** Drop the user's edits of one provider; a provider the composition defines stays. */
export async function removeProvider(ctx: HostContext, provider: string): Promise<{ removed: boolean }> {
  const settings = settingsOf(ctx)
  if (settings.writable !== true) throw new Error('dsh settings are read-only in this profile')
  await settings.mutate(PROVIDERS_NS, [{ op: 'unset', path: ['providers', provider] }])
  return { removed: !(provider in resolved(settings)) }
}
