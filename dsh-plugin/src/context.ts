/** hi/luna 的图像上下文预算；原图和观察仍由 DSH 的持久会话持有。 */
export const CONTEXT_REPAIR_VERSION = 2
const LEDGER = 'ZF_IMAGE_OBSERVATIONS_V1\n'
const MAX_LEDGER_BYTES = 192 * 1024
const MAX_IMAGES = 512
const HEADROOM = 16384

interface ImageRef {
  attachmentId: string
  width: number
  height: number
  mediaType: string
  bytes: number
}
interface Block {
  type: string
  text?: string
  attachment?: ImageRef
  offloaded?: boolean
  [key: string]: unknown
}
interface Message {
  id?: string
  role: string
  source?: { kind?: string }
  toolCallId?: string
  content: readonly Block[]
}
interface Observation {
  messageId: string
  text: string
}
interface ImageObservation {
  messageId: string
  images: ImageRef[]
  receipt: string
  observations: Observation[]
  reader?: { callId: string; name: string; arguments: string }
}
interface Pricing {
  priceImages(images: readonly Block[]): readonly { visualTokens: number; text: string }[]
}
export interface ImagePricingHelpers {
  dimensions(width: number, height: number): { width: number; height: number }
  handle(ref: ImageRef, dimensions: { width: number; height: number }): string
  omitted(ref: ImageRef): string
}
interface Request {
  provider: string
  model: string
  messages: readonly Message[]
  tools?: readonly unknown[]
  system?: string
  maxTokens?: number
  purpose?: string
}

/** 供应商实报仍溢出时，有限次移出已观察前缀；未观察图像继续阻止重试。 */
export function imageOverflowOffload(request: Request, failureCode: string): number {
  if (failureCode !== 'CONTEXT_WINDOW_EXCEEDED') return 0
  return Math.ceil(observedImagePrefix(request) / 2)
}

/** 所有卸载来源（包括原适配器的字节上限）必须满足同一个前缀约束。 */
export function assertObservedImageOffload(request: Request, count: number): void {
  if (!Number.isSafeInteger(count) || count <= 0 || count > observedImagePrefix(request)) {
    throw new Error('image context byte or token budget requires unobserved images; preserve the complete pending batch')
  }
}

function observedImagePrefix(request: Request): number {
  const observed = new Set(observations(request.messages).filter(entry => entry.observations.length > 0).map(entry => entry.messageId))
  let prefix = 0
  for (const message of request.messages) {
    if (message.role !== 'tool' && message.role !== 'user') continue
    for (const block of message.content) {
      if (block.type !== 'image' || block.offloaded === true) continue
      if (message.id === undefined || !observed.has(message.id)) return prefix
      prefix += 1
    }
  }
  return prefix
}

export function repairedRoute(provider: string, model: string): boolean {
  return provider === 'hi' && model === 'gpt-6-luna'
}

/** 同一请求投影供 meter 和入站预算使用；并非供应商的计费 tokenizer。 */
export function imageRequestPricing(helpers: ImagePricingHelpers): Pricing {
  return { priceImages: images => images.map(block => {
    const ref = block.attachment
    if (ref === undefined || !Number.isSafeInteger(ref.width) || ref.width <= 0 || !Number.isSafeInteger(ref.height) || ref.height <= 0) {
      throw new Error('image context: invalid attachment dimensions')
    }
    if (block.offloaded === true) return { visualTokens: 0, text: helpers.omitted(ref) }
    const target = helpers.dimensions(ref.width, ref.height)
    // 32px 网格乘二作保守视觉估价；使用真正发往模型的尺寸，不能计 base64 或引用长度。
    const visualTokens = Math.ceil(target.width / 32) * Math.ceil(target.height / 32) * 2
    if (!Number.isSafeInteger(visualTokens) || visualTokens <= 0) throw new Error('image context: invalid request dimensions')
    return { visualTokens, text: helpers.handle(ref, target) }
  }) }
}

function text(message: Message): string {
  return message.content.filter(block => block.type === 'text').map(block => block.text ?? '').join('\n')
}

function observations(messages: readonly Message[]): ImageObservation[] {
  const entries = new Map<string, ImageObservation>()
  const readers = new Map<string, { callId: string; name: string; arguments: string }>()
  let pending: ImageObservation[] = []
  for (const message of messages) {
    if (message.role === 'assistant') {
      for (const block of message.content) {
        if (block.type === 'tool-call' && typeof block['id'] === 'string' && typeof block['name'] === 'string' && typeof block['arguments'] === 'string') {
          readers.set(block['id'], { callId: block['id'], name: block['name'], arguments: block['arguments'] })
        }
      }
    }
    if (message.role === 'user' && message.source?.kind === 'compact-checkpoint') {
      for (const block of message.content) {
        if (block.type !== 'text' || !block.text?.startsWith(LEDGER)) continue
        if (Buffer.byteLength(block.text) > MAX_LEDGER_BYTES + LEDGER.length) throw new Error('image context: observation checkpoint exceeds its bound')
        const saved: unknown = JSON.parse(block.text.slice(LEDGER.length))
        if (!Array.isArray(saved)) throw new Error('image context: invalid observation checkpoint')
        for (const entry of saved as ImageObservation[]) {
          if (typeof entry.messageId !== 'string' || !Array.isArray(entry.images) || !Array.isArray(entry.observations) || typeof entry.receipt !== 'string') {
            throw new Error('image context: invalid observation checkpoint')
          }
          const copy = structuredClone(entry)
          entries.set(entry.messageId, copy)
          pending = [copy]
        }
      }
    }
    const images = message.role === 'tool' || message.role === 'user'
      ? message.content.filter(block => block.type === 'image').map(block => block.attachment!) : []
    if (images.length > 0) {
      if (typeof message.id !== 'string' || images.some(ref => !/^sha256:[0-9a-f]{64}$/.test(ref?.attachmentId ?? ''))) {
        throw new Error('image context: image identity is missing')
      }
      if (pending.some(entry => entry.observations.length > 0)) pending = []
      const entry = entries.get(message.id) ?? {
        messageId: message.id, images: structuredClone(images), receipt: text(message), observations: [],
      }
      const reader = message.toolCallId === undefined ? undefined : readers.get(message.toolCallId)
      if (reader !== undefined) entry.reader = reader
      entries.set(message.id, entry)
      pending.push(entry)
    } else if (message.role === 'assistant' && pending.length > 0) {
      const body = text(message)
      if (body.trim() !== '') {
        if (typeof message.id !== 'string') throw new Error('image context: observation identity is missing')
        for (const entry of pending) {
          if (!entry.observations.some(observation => observation.messageId === message.id)) {
            entry.observations.push({ messageId: message.id, text: body })
          }
        }
      }
    }
  }
  const result = [...entries.values()]
  if (result.reduce((count, entry) => count + entry.images.length, 0) > MAX_IMAGES || Buffer.byteLength(JSON.stringify(result)) > MAX_LEDGER_BYTES) {
    throw new Error('image context: observation checkpoint exceeds its bound; preserve review records before continuing')
  }
  return result
}

/** 摘要不能改写已绑定图像身份的观察；保留原工具回执中的 build/evidence/reader。 */
export function preserveImageObservations(summary: readonly Block[], messages: readonly Message[]): readonly Block[] {
  const entries = observations(messages)
  const body = summary.filter(block => !block.text?.startsWith(LEDGER))
  return entries.length === 0 ? body : [...body, { type: 'text', text: LEDGER + JSON.stringify(entries) }]
}

function blockTokens(block: Block): number {
  if (block.type === 'text') return Math.ceil((block.text ?? '').length / 4) + 4
  return Math.ceil(JSON.stringify(block).length / 4) + 4
}

/** 只返回最老的已观察图像前缀；调用方通过宿主 image/offload 事件持久化。 */
export function boundImageRequest(request: Request, contextWindow: number, pricing: Pricing): {
  inputTokens: number; maxTokens: number; offloadImages: number
} {
  if (!Number.isSafeInteger(contextWindow) || contextWindow <= HEADROOM) throw new Error('image context: invalid model capacity')
  const maxTokens = Math.min(request.maxTokens ?? 16384, request.purpose === 'compaction' ? 8192 : 16384)
  if (!Number.isSafeInteger(maxTokens) || maxTokens <= 0) throw new Error('image context: invalid output budget')
  const observed = new Set(observations(request.messages).filter(entry => entry.observations.length > 0).map(entry => entry.messageId))
  const images: { messageId: string | undefined; block: Block; cost: number; omittedCost: number }[] = []
  let inputTokens = Math.ceil(JSON.stringify(request.tools ?? []).length / 4) + Math.ceil((request.system ?? '').length / 4)
  for (const message of request.messages) {
    inputTokens += 4
    for (const block of message.content) {
      if (block.type !== 'image') { inputTokens += blockTokens(block); continue }
      const price = pricing.priceImages([block])[0]!
      const cost = price.visualTokens + Math.ceil(price.text.length / 4) + 4
      inputTokens += cost
      if (block.offloaded !== true) {
        const omitted = pricing.priceImages([{ ...block, offloaded: true }])[0]!
        images.push({ messageId: message.id, block, cost, omittedCost: Math.ceil(omitted.text.length / 4) + 4 })
      }
    }
  }
  const budget = contextWindow - maxTokens - HEADROOM
  let offloadImages = 0
  for (const image of images) {
    if (inputTokens <= budget) break
    if (image.messageId === undefined || !observed.has(image.messageId)) break
    inputTokens -= image.cost - image.omittedCost
    offloadImages += 1
  }
  if (inputTokens > budget) throw new Error('image context budget cannot fit unobserved images or text; preserve the pending batch and use a fresh task context')
  return { inputTokens, maxTokens, offloadImages }
}
