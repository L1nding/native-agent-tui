// 仅修补已核对的宿主版本；升级或外部修改后拒绝覆盖，重新审查接缝。
import { createHash, randomUUID } from 'node:crypto'
import * as fs from 'node:fs'
import { createRequire } from 'node:module'
import { resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const context = pathToFileURL(fileURLToPath(new URL('../dsh-plugin/lib/context.js', import.meta.url))).href
const hash = value => createHash('sha256').update(value).digest('hex')
const replace = (source, before, after) => {
  if (source.split(before).length !== 2) throw new Error('DSH context repair: host seam is missing or ambiguous')
  return source.replace(before, after)
}
const repairs = [
  {
    name: '@deepseek-ai/dsh-mcp-client',
    original: '758d8fc473b700b5d351158d06ce9f9e92c16e8b25497b08bb7bd2122a87f5dd',
    apply(source) {
      source = replace(source,
        'import { Client, StreamableHTTPClientTransport, specTypeSchemas } from "@modelcontextprotocol/client";',
        'import { Client, StreamableHTTPClientTransport, SdkHttpError, specTypeSchemas } from "@modelcontextprotocol/client";')
      source = replace(source, '\t\tconst hasClosed = () => closeObserved;', `
    let retiring = false, closing;
    const originalCallTool = generation.callTool.bind(generation);
    generation.callTool = async (...args) => {
      if (!isCurrent(generation) || retiring) throw new Error(label + ": server generation is retired or disconnected; call not dispatched");
      try {
        return await originalCallTool(...args);
      } catch (error) {
        let sessionMissing = false;
        if (config.transport === "streamable-http" && error instanceof SdkHttpError && error.status === 404
          && typeof error.data?.text === "string" && error.data.text.length <= 4096) {
          try {
            const body = JSON.parse(error.data.text);
            sessionMissing = body.jsonrpc === "2.0" && body.error?.code === -32600 && body.error?.message === "Session not found";
          } catch {}
        }
        if (!sessionMissing) throw error;
        // 404 只更新连接归属，失败调用无论读写都不能自动重放。
        if (isCurrent(generation) && !retiring) {
          retiring = true;
          const quiesced = await closeGeneration();
          settleFailedGeneration(generation, quiesced);
        }
        throw new Error(error.message + "; call not replayed; prior side effects remain unconfirmed, inspect durable state before retrying", { cause: error });
      }
    };
		const hasClosed = () => closeObserved;`)
      source = replace(source, 'if (attemptSettled) generationDown(generation);', 'if (attemptSettled && !retiring) generationDown(generation);')
      source = replace(source, `async function closeGeneration() {
			const attached`, `async function closeGeneration() {
      return closing ??= closeGenerationOnce();
    }
    async function closeGenerationOnce() {
			const attached`)
      return source
    },
  },
  {
    name: '@deepseek-ai/dsh-acp',
    original: 'c08ed51ebe53b7b45ca8f475630afc0271bf4ccb8b53f13b456fe2b4568a61b3',
    apply(source) {
      source = 'export const ZF_TUI_LIFECYCLE_VERSION = 1;\n' + source
      source = replace(source, '\tonSessionEvent(session, event) {', `
  zfNotifyUpdate(event, update) {
    return this.notify({ sessionId: this.agent.session.id, update,
      _meta: { zfHostTurn: event.data.turn, zfEventSeq: event.seq } });
  }
\tonSessionEvent(session, event) {`)
      source = replace(source, `await this.notify({
\t\t\t\t\t\tsessionId: this.agent.session.id,
\t\t\t\t\t\tupdate
\t\t\t\t\t});`, 'await this.zfNotifyUpdate(event, update);')
      source = replace(source, `this.notify({
\t\t\t\t\tsessionId: this.agent.session.id,
\t\t\t\t\tupdate: toolCallUpdate(event)
\t\t\t\t})`, 'this.zfNotifyUpdate(event, toolCallUpdate(event))')
      source = replace(source, `this.notify({
\t\t\t\t\tsessionId: this.agent.session.id,
\t\t\t\t\tupdate: await toolResultUpdate(this.ctx, event)
\t\t\t\t})`, 'this.zfNotifyUpdate(event, await toolResultUpdate(this.ctx, event))')
      for (const subject of ['assistant output conversion', 'tool-call update delivery', 'tool-result update delivery']) {
        const seam = `this.ctx.logger.warn(\`acp: ${subject} failed: \${errorChain(error)}\`);`
        source = replace(source, seam, `this.zfFailedTurn = event.data.turn;\n\t\t\t\t\t${seam}`)
      }
      source = replace(source, 'if (event.type === "turn/end") this.modelControl.releaseTurn(event.data.turn);', `if (event.type === "turn/end") {
        this.modelControl.releaseTurn(event.data.turn);
        // 必须排空本轮 ACP 输出后才发布终态，不能用原始 turn/end 代替。
        this.outputTail = this.outputTail.then(() => {
          const state = this.zfFailedTurn === event.data.turn ? "failed"
            : event.data.reason?.kind === "completed" ? "completed"
            : event.data.reason?.kind === "aborted" ? "interrupted" : "failed";
          // 只传封闭类别和错误码，不把 provider 原文带入界面协议。
          const failureCategory = this.zfFailedTurn === event.data.turn ? "output_delivery"
            : event.data.reason?.kind === "error" ? "provider" : "unknown";
          const rawCode = event.data.reason?.error?.code;
          const code = rawCode === "QUOTA" ? "QUOTA_EXCEEDED" : rawCode;
          const failureCode = failureCategory === "provider" && ["AUTH", "QUOTA_EXCEEDED", "RATE_LIMIT", "SERVER", "TIMEOUT", "TRANSPORT", "PI_AI_ERROR", "CONTEXT_WINDOW_EXCEEDED", "EMPTY_RESPONSE"].includes(code) ? code : undefined;
          return this.ctx.emit("zf-tui/acp-turn-settled", {
            sessionId: this.agent.session.id, turn: event.data.turn, state,
            ...(state === "failed" ? { failureCategory, ...(failureCode === undefined ? {} : { failureCode }) } : {})
          });
        });
      }`)
      return source
    },
  },
  {
    name: '@deepseek-ai/dsh-llm-pi-ai',
    original: '1dd20bf74f4daca571ae1d6ea55986bc4e7bfa8c3dd28c9f1939a1861fb2b774',
    apply(source, context) {
      source = replace(source,
        String.raw`if (/\b429\b|rate.?limit/i.test(message)) return "RATE_LIMIT";`,
        String.raw`// 网关并发限流可能没有 HTTP 429，沿用宿主有限重试。
	if (/\b429\b|rate.?limit|\bgateway_concurrency_limit\b|\bConcurrency limit exceeded for user\b/i.test(message)) return "RATE_LIMIT";`)
      source = `import { repairedRoute as zfRepairedRoute, imageRequestPricing as zfImageRequestPricing, boundImageRequest as zfBoundImageRequest, imageOverflowOffload as zfImageOverflowOffload, assertObservedImageOffload as zfAssertObservedImageOffload } from ${JSON.stringify(context)};\n` + source
      source = replace(source, 'var PiAiAdapter = class extends LlmAdapter {', `var PiAiAdapter = class extends LlmAdapter {
  zfContextRepairVersion() { return 2; }
  imageRequestPricing(provider, model) {
    if (!zfRepairedRoute(provider, model)) return super.imageRequestPricing(provider, model);
    return this.zfImagePricing(this.current(), provider, model);
  }
  zfImagePricing(snapshot, provider, model) {
    const profile = this.profileOf(snapshot, provider);
    const attachments = this.config.resolveAttachments?.();
    const access = ref => attachments === undefined ? undefined : this.config.resolveImageAccess?.(attachments, ref);
    return zfImageRequestPricing({
      dimensions: (width, height) => requestImageDimensions(width, height, profile.requestImagePixelBudget),
      handle: (ref, dimensions) => requestImageHandleText(ref, dimensions, access(ref)),
      omitted: ref => offloadedImageText(ref, access(ref))
    });
  }`)
      source = replace(source,
        '...configuredMaxTokens === void 0 ? {} : { defaultMaxTokens: configuredMaxTokens },',
        '...configuredMaxTokens === void 0 ? {} : { defaultMaxTokens: zfRepairedRoute(provider, model) ? Math.min(configuredMaxTokens, 16384) : configuredMaxTokens },')
      source = replace(source,
        '\t\t\tconst model = this.modelOf(snapshot, options.provider, options.model);',
        `\t\t\tconst model = this.modelOf(snapshot, options.provider, options.model);
      if (zfRepairedRoute(options.provider, options.model)) {
        const budget = zfBoundImageRequest(options, model.contextWindow, this.zfImagePricing(snapshot, options.provider, options.model));
        if (budget.offloadImages > 0) throw new LlmError("image context: oldest observed images must leave the next request", IMAGE_OFFLOAD_REQUIRED_CODE, { offloadImages: budget.offloadImages });
        options = { ...options, maxTokens: budget.maxTokens };
      }`)
      source = replace(source, '\t\t\t\t\t\tyield result.value;', `
            if (zfRepairedRoute(options.provider, options.model) && result.value.type === "finish" && result.value.reason.kind === "error") {
              if (result.value.reason.failure.code === IMAGE_OFFLOAD_REQUIRED_CODE) throw new LlmError(result.value.reason.failure.message, IMAGE_OFFLOAD_REQUIRED_CODE, result.value.reason.failure);
              const offloadImages = options.signal?.aborted ? 0 : zfImageOverflowOffload(options, result.value.reason.failure.code);
              if (offloadImages > 0) throw new LlmError("image context: provider context overflow requires offloading observed history", IMAGE_OFFLOAD_REQUIRED_CODE, { offloadImages });
            }
\t\t\t\t\t\tyield result.value;`)
      source = replace(source, '\t\t} catch (e_1) {\n\t\t\tenv_1.error = e_1;', `\t\t} catch (e_1) {
      let failure = e_1;
      if (zfRepairedRoute(options.provider, options.model) && e_1 instanceof LlmError && e_1.code === IMAGE_OFFLOAD_REQUIRED_CODE) {
        try {
          if (options.signal?.aborted) throw new LlmError("image context request aborted", "ABORTED", { cause: e_1 });
          zfAssertObservedImageOffload(options, e_1.failure.offloadImages);
        } catch (error) {
          failure = error instanceof LlmError ? error : new LlmError(error.message, "UNOBSERVED_IMAGE_BUDGET", { cause: e_1 });
        }
      }
\t\t\tenv_1.error = failure;`)
      return source
    },
  },
  {
    name: '@deepseek-ai/dsh-compaction-basic',
    original: 'b7ac52f20a36430cf01a66b3a36f23a48ffdb22708d714dbe1fff541b5df4ec5',
    apply(source, context) {
      source = `import { repairedRoute as zfRepairedRoute, preserveImageObservations as zfPreserveImageObservations } from ${JSON.stringify(context)};\nexport const ZF_CONTEXT_REPAIR_VERSION = 2;\n` + source
      source = replace(source, '\t\tmaxTokens: config.maxTokens,\n\t\tsessionId:', '\t\tmaxTokens: zfRepairedRoute(target.provider, target.model) ? Math.min(config.maxTokens, 8192) : config.maxTokens,\n\t\tsessionId:')
      source = replace(source, '\tconst summary = summaryText(rawOutput);', '\tconst summary = zfRepairedRoute(target.provider, target.model) ? zfPreserveImageObservations(summaryText(rawOutput), input.messages) : summaryText(rawOutput);')
      source = replace(source, '\t\tmaxTokens: config.maxTokens,\n\t\t...assembler.usage', '\t\tmaxTokens: options.maxTokens,\n\t\t...assembler.usage')
      return source
    },
  },
]

// 只生成计划，不加载/启动宿主；离线验证使用同一份接缝生成结果。
export function prepareRepairs(hostEntry, contextUrl = context) {
  const fromHost = createRequire(resolve(hostEntry))
  return repairs.map(repair => {
    const path = fromHost.resolve(repair.name)
    const originalBytes = fs.readFileSync(path)
    const current = originalBytes.toString('utf8')
    const marker = '\n// zf-context-repair-original:'
    const boundary = current.lastIndexOf(marker)
    const original = boundary < 0 ? current : Buffer.from(current.slice(boundary + marker.length).split('\n')[0], 'base64').toString('utf8')
    if (hash(original) !== repair.original) throw new Error(`DSH context repair: ${repair.name} source changed; refusing to overwrite`)
    if (boundary >= 0 && hash(current.slice(0, boundary)) !== current.split('// zf-context-repair-body-sha256:')[1]?.trim()) {
      throw new Error(`DSH context repair: ${repair.name} patched source changed; refusing to overwrite`)
    }
    const generated = repair.apply(original, contextUrl)
    // 原字节只作注释保存，用于幂等校验和升级保护，不导出或执行第二份宿主代码。
    const tagged = generated + marker + Buffer.from(original).toString('base64') + `\n// zf-context-repair-body-sha256:${hash(generated)}\n`
    return { path, originalBytes, changed: current !== tagged, content: tagged }
  })
}

/** 先完整暂存全部新内容和原字节，再逐文件原子替换；提交失败恢复全部原字节。 */
export function commitRepairs(plans, { check = false, io = fs } = {}) {
  if (check) {
    for (const plan of plans) if (hash(io.readFileSync(plan.path)) !== hash(plan.content)) throw new Error('DSH context repair is not installed')
    return
  }
  const staged = []
  const attempted = []
  const retained = new Set()
  let failure
  try {
    for (const plan of plans.filter(plan => plan.changed)) {
      if (hash(io.readFileSync(plan.path)) !== hash(plan.originalBytes)) throw new Error('DSH context repair: host changed since preflight')
      const suffix = `.zf-context-${randomUUID()}`
      const entry = { ...plan, temporary: plan.path + suffix + '.tmp', backup: plan.path + suffix + '.bak' }
      staged.push(entry)
      io.writeFileSync(entry.backup, entry.originalBytes, { flag: 'wx' })
      if (hash(io.readFileSync(entry.backup)) !== hash(entry.originalBytes)) throw new Error('DSH context repair: backup verification failed')
      io.writeFileSync(entry.temporary, entry.content, { flag: 'wx' })
      if (hash(io.readFileSync(entry.temporary)) !== hash(entry.content)) throw new Error('DSH context repair: staged write verification failed')
    }
    for (const entry of staged) {
      if (hash(io.readFileSync(entry.path)) !== hash(entry.originalBytes)) throw new Error('DSH context repair: host changed before commit')
      attempted.push(entry)
      io.renameSync(entry.temporary, entry.path)
      if (hash(io.readFileSync(entry.path)) !== hash(entry.content)) throw new Error('DSH context repair: committed write verification failed')
    }
  } catch (error) {
    const failures = [error]
    for (const entry of attempted) {
      try {
        io.renameSync(entry.backup, entry.path)
        if (hash(io.readFileSync(entry.path)) !== hash(entry.originalBytes)) throw new Error('restored bytes differ')
      } catch (restoreError) {
        retained.add(entry.backup)
        failures.push(new Error(`DSH context repair: restore failed for ${entry.path}; original bytes retained at ${entry.backup}`, { cause: restoreError }))
      }
    }
    failure = failures.length > 1 ? new AggregateError(failures, 'DSH context repair commit and restore failed') : error
  } finally {
    for (const entry of staged) for (const path of [entry.temporary, entry.backup]) {
      if (!retained.has(path)) {
        try { io.unlinkSync(path) } catch (error) {
          if (error.code !== 'ENOENT') failure = new AggregateError([...(failure ? [failure] : []), error], `DSH context repair cleanup failed for ${path}`)
        }
      }
    }
  }
  if (failure) throw failure
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2)
  const index = args.indexOf('--host-entry')
  if (index < 0 || !args[index + 1]) throw new Error('Usage: node scripts/install-dsh-context.mjs --host-entry <dsh/lib/bin.js> [--check]')
  if ((await import(context)).CONTEXT_REPAIR_VERSION !== 2) throw new Error('Build dsh-plugin before installing the context repair')
  const plans = prepareRepairs(args[index + 1])
  commitRepairs(plans, { check: args.includes('--check') })
  for (const plan of plans) console.log(`${plan.changed ? 'installed' : 'verified'} ${plan.path} sha256=${hash(plan.content)}`)
}
