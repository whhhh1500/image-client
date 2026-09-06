#!/usr/bin/env node
/**
 * Purpose-built loopback proxy for one normal-window comic acceptance run.
 *
 * It is deliberately not a general API proxy: only two frozen LLM request
 * shapes and one no-reference image request are eligible.  Credentials and
 * raw prompts/responses stay in memory; durable files contain only safe event
 * kinds, integer statuses, request digests and immutable scope identifiers.
 */

import assert from 'node:assert/strict';
import { createHash, randomBytes, timingSafeEqual } from 'node:crypto';
import { createServer, request as httpRequest } from 'node:http';
import { request as httpsRequest } from 'node:https';
import {
  closeSync,
  existsSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { dirname, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const DB_GATE = resolve(here, 'db_gate.py');
const MAX_BODY_BYTES = 1024 * 1024;
const MAX_IMAGE_RESPONSE_BYTES = 20 * 1024 * 1024;
const KINDS = new Set(['source', 'adaptation', 'image']);
const LOOPBACK = new Set(['127.0.0.1', '::1', '::ffff:127.0.0.1']);
const SOURCE_SYSTEM_PREFIX = '你是小说分析器。仅输出一个聚合 JSON：{"artifacts":[...14 items...]}，不得 Markdown 或解释。artifacts 必须恰有 14 项、每种 artifactType 一项；每一项都须分别符合下列冻结 Schema root oneOf，包含 schemaVersion/owner/content/warnings。不得生成 canon_diff；它只能由已采用 canon delta 确定性派生。\n';
const ADAPTATION_SYSTEM_PREFIX = '你是改编规划分析器。只输出聚合 JSON {"artifacts":[...4 items...]}，没有 Markdown 或解释。artifacts 必须恰有 adaptation_proposal、comic_chapter_plan、scene_plan、page_panel_plan 各一项，绝不能输出十类原著分析。每项必须包含 schemaVersion=\'novel-analysis.v1\'、artifactType、owner、content、warnings；ownerMap 是强制精确值。每项 content 必须符合下面冻结的 Novel Analysis v1 Schema 相应 oneOf，page_panel_plan 还必须有可验证的完整 geometry。';

export const PROXY_CONTRACT = 'normal-ui-acceptance-proxy.v1';

function sha256(value) {
  return `sha256:${createHash('sha256').update(value).digest('hex')}`;
}

function safeCode(value, fallback = 'UI_PROXY_REJECTED') {
  return typeof value === 'string' && /^[A-Z0-9_]{3,96}$/.test(value)
    ? value
    : fallback;
}

function fail(code, status = 502) {
  const error = new Error(safeCode(code));
  error.status = status;
  return error;
}

function isLoopback(remote) {
  return typeof remote === 'string' && LOOPBACK.has(remote);
}

function strictEnv(text) {
  const values = new Map();
  for (const original of text.split(/\r?\n/)) {
    const line = original.trim();
    if (!line || line.startsWith('#')) continue;
    if (/^export\s/i.test(line) || /\$\{|\$\(|[`]/.test(line)) {
      throw fail('UI_PROXY_REAL_ENV_COMPLEX_SYNTAX');
    }
    const match = /^([A-Za-z_][A-Za-z0-9_.-]*)\s*=\s*(.*)$/.exec(line);
    if (!match) throw fail('UI_PROXY_REAL_ENV_SYNTAX');
    const key = match[1].toUpperCase().replaceAll('-', '_');
    const value = match[2];
    if (/^['"]|['"]$/.test(value)) throw fail('UI_PROXY_REAL_ENV_COMPLEX_SYNTAX');
    if (values.has(key)) throw fail('UI_PROXY_REAL_ENV_DUPLICATE_KEY');
    values.set(key, value);
  }
  return values;
}

export function readRealProviderEnv(path) {
  const env = strictEnv(readFileSync(path, 'utf8'));
  const required = ['LLM_API_URL', 'LLM_API_KEY', 'IMAGE_API_URL', 'IMAGE_API_KEY'];
  for (const key of required) {
    if (!env.get(key)?.trim()) throw fail('UI_PROXY_REAL_ENV_REQUIRED');
  }
  return {
    llmUrl: completionUrl(env.get('LLM_API_URL')),
    llmKey: env.get('LLM_API_KEY'),
    imageUrl: imageUrl(env.get('IMAGE_API_URL')),
    imageKey: env.get('IMAGE_API_KEY'),
    llmModel: env.get('LLM_API_MODEL') || 'gemini-3.7-flash',
    imageModel: env.get('IMAGE_API_MODEL') || 'gpt-image-2',
  };
}

function completionUrl(value) {
  const base = new URL(value.trim());
  if (!['http:', 'https:'].includes(base.protocol)) throw fail('UI_PROXY_LLM_URL_SCHEME');
  base.hash = '';
  base.search = '';
  base.pathname = base.pathname.replace(/\/+$/, '');
  if (!base.pathname.endsWith('/chat/completions')) base.pathname += '/chat/completions';
  return base;
}

function imageUrl(value) {
  const url = new URL(value.trim());
  if (!['http:', 'https:'].includes(url.protocol)) throw fail('UI_PROXY_IMAGE_URL_SCHEME');
  if (!url.pathname.endsWith('/images/generations') || url.search || url.hash) {
    throw fail('UI_PROXY_IMAGE_URL_INVALID');
  }
  return url;
}

function safeJsonParse(buffer) {
  try {
    const value = JSON.parse(buffer.toString('utf8'));
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error();
    return value;
  } catch {
    throw fail('UI_PROXY_JSON_REQUIRED', 400);
  }
}

function readRequestBody(req) {
  return new Promise((resolveBody, reject) => {
    const chunks = [];
    let bytes = 0;
    req.on('data', (chunk) => {
      bytes += chunk.length;
      if (bytes > MAX_BODY_BYTES) {
        reject(fail('UI_PROXY_BODY_TOO_LARGE', 413));
        req.destroy();
      } else {
        chunks.push(chunk);
      }
    });
    req.on('error', reject);
    req.on('end', () => resolveBody(Buffer.concat(chunks)));
  });
}

function exactToken(actual, expected) {
  if (typeof actual !== 'string' || !actual.startsWith('Bearer ')) return false;
  const received = Buffer.from(actual.slice(7));
  const wanted = Buffer.from(expected);
  return received.length === wanted.length && timingSafeEqual(received, wanted);
}

function systemAndUser(body) {
  if (body.stream !== true || body.stream_options?.include_usage !== true) {
    throw fail('UI_PROXY_STREAM_REQUIRED');
  }
  if (!Array.isArray(body.messages) || body.messages.length !== 2 || body.tools !== undefined) {
    throw fail('UI_PROXY_MESSAGE_SHAPE_INVALID');
  }
  const [system, user] = body.messages;
  if (system?.role !== 'system' || typeof system.content !== 'string' || user?.role !== 'user' || typeof user.content !== 'string') {
    throw fail('UI_PROXY_MESSAGE_SHAPE_INVALID');
  }
  return { system: system.content, user: safeJsonParse(Buffer.from(user.content)) };
}

export function classifyLlm(body, baseScope) {
  const { system, user } = systemAndUser(body);
  const owner = user.ownerMap;
  if (!owner || typeof owner !== 'object' || Array.isArray(owner)) throw fail('UI_PROXY_OWNER_SCOPE_MISSING');
  const source = system.startsWith(SOURCE_SYSTEM_PREFIX)
    && system.includes('novel-analysis.v1')
    && typeof user.chapterContent === 'string'
    && typeof user.chapterContentHash === 'string';
  if (source) {
    if (owner.novel_work !== baseScope.novelWorkId || owner.novel_chapter_revision !== baseScope.sourceRevisionId) {
      throw fail('UI_PROXY_SOURCE_OWNER_SCOPE_MISMATCH');
    }
    return { kind: 'source', scope: { ...baseScope } };
  }
  const adaptation = system.startsWith(ADAPTATION_SYSTEM_PREFIX)
    && system.includes('正式页面生产合同')
    && system.includes('outputIdentityBindings')
    && typeof user.runId === 'string'
    && Array.isArray(user.frozenOriginalArtifacts)
    && user.frozenOriginalArtifacts.length === 10
    && user.comicPlanIntent && typeof user.comicPlanIntent === 'object';
  if (adaptation) {
    if (user.novelWorkId !== baseScope.novelWorkId
      || typeof owner.comic_adaptation !== 'string'
      || typeof owner.comic_chapter !== 'string') {
      throw fail('UI_PROXY_ADAPTATION_OWNER_SCOPE_MISMATCH');
    }
    return {
      kind: 'adaptation',
      scope: {
        ...baseScope,
        adaptationAnalysisRunId: user.runId,
        comicAdaptationId: owner.comic_adaptation,
        comicAdaptationChapterId: owner.comic_chapter,
      },
    };
  }
  throw fail('UI_PROXY_LLM_CONTRACT_UNRECOGNIZED');
}

export function validateImageBody(body) {
  // The normal comic path does not declare a quality/background surcharge.
  // Keep the acceptance proxy narrower than the provider surface instead of
  // passing through optional price- or rendering-affecting switches.
  const allowed = new Set(['model', 'prompt', 'n', 'size']);
  if (Object.keys(body).some((key) => !allowed.has(key))
    || typeof body.model !== 'string'
    || typeof body.prompt !== 'string'
    || body.n !== 1
    || body.size !== '1024x1536'
    || body.prompt.length === 0) {
    throw fail('UI_PROXY_IMAGE_REQUEST_INVALID');
  }
  return body;
}

function readJsonFile(path, code) {
  try {
    const value = JSON.parse(readFileSync(path, 'utf8'));
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error();
    return value;
  } catch {
    throw fail(code);
  }
}

function verifyScope(value) {
  const fields = ['projectId', 'novelWorkId', 'novelChapterId', 'sourceRevisionId', 'productionJobId', 'sourceAnalysisRunId'];
  if (value.schemaVersion !== 'normal-ui-acceptance-scope.v1'
    || fields.some((field) => typeof value[field] !== 'string' || !value[field])) {
    throw fail('UI_PROXY_SCOPE_INVALID');
  }
  return Object.fromEntries(fields.map((field) => [field, value[field]]));
}

function atomicJson(path, value) {
  const parent = dirname(path);
  mkdirSync(parent, { recursive: true });
  const temporary = `${path}.${randomBytes(8).toString('hex')}.tmp`;
  const fd = openSync(temporary, 'w', 0o600);
  try {
    writeFileSync(fd, JSON.stringify(value));
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  renameSync(temporary, path);
}

function appendFsync(path, event) {
  mkdirSync(dirname(path), { recursive: true });
  const fd = openSync(path, 'a', 0o600);
  try {
    writeFileSync(fd, `${JSON.stringify(event)}\n`);
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
}

function readEvents(path) {
  if (!existsSync(path)) return [];
  return readFileSync(path, 'utf8').split(/\r?\n/).filter(Boolean).flatMap((line) => {
    try {
      const value = JSON.parse(line);
      return value && typeof value === 'object' ? [value] : [];
    } catch {
      // A crash may leave only the final write partial. It can never lower a
      // quota because an omitted/invalid event is treated as unsafe below.
      return [{ event: 'audit_corrupt' }];
    }
  });
}

function oneTimeUsed(events, kind) {
  return events.some((event) => event.event === 'generation_post_reserved' && event.kind === kind)
    || events.some((event) => event.event === 'audit_corrupt');
}

function runDbGate(options, kind, scope) {
  const safeScopePath = `${options.runtimeRoot}/gate-${kind}-${randomBytes(8).toString('hex')}.json`;
  try {
    atomicJson(safeScopePath, scope);
    const result = spawnSync(options.pythonPath, [DB_GATE, '--database', options.databasePath, '--kind', kind, '--scope', safeScopePath], {
      encoding: 'utf8',
      timeout: options.dbGateTimeoutMs,
      windowsHide: true,
    });
    if (result.error || result.status !== 0) throw fail('UI_PROXY_DB_GATE_UNAVAILABLE');
    const parsed = JSON.parse(result.stdout);
    if (!parsed.ok) throw fail(safeCode(parsed.code, 'UI_PROXY_DB_GATE_REJECTED'));
    return parsed.result;
  } catch (error) {
    if (error?.status) throw error;
    throw fail('UI_PROXY_DB_GATE_UNAVAILABLE');
  } finally {
    if (existsSync(safeScopePath)) rmSync(safeScopePath, { force: true });
  }
}

function briefImageSnapshot(gate, body) {
  return {
    schemaVersion: 'normal-ui-image-review.v1',
    jobId: gate.jobId,
    batchId: gate.batchId,
    memberId: gate.memberId,
    manifestId: gate.manifestId,
    manifestFingerprint: gate.manifestFingerprint,
    soundEffectCount: gate.soundEffectCount,
    dialogueChars: gate.dialogueChars,
    formalManifestReview: gate.reviewSnapshot,
    requestDigest: sha256(JSON.stringify(body)),
  };
}

function stableImageReviewSnapshot(snapshot) {
  const { checkpointCreatedAtUtc, reviewExpiresAtUtc, ...stable } = snapshot;
  return stable;
}

function reviewWindow(snapshot, configuredMs) {
  const created = Date.parse(snapshot.checkpointCreatedAtUtc);
  const expires = Date.parse(snapshot.reviewExpiresAtUtc);
  if (!Number.isFinite(created) || !Number.isFinite(expires) || expires <= created
    || expires - created !== configuredMs) {
    throw fail('UI_PROXY_IMAGE_CHECKPOINT_INVALID');
  }
  return { created, expires };
}

function safeResponse(res, status, code) {
  res.statusCode = status;
  res.setHeader('content-type', 'application/json; charset=utf-8');
  res.setHeader('cache-control', 'no-store');
  res.end(JSON.stringify({ error: { code: safeCode(code), message: 'normal-ui acceptance proxy rejected the request' } }));
}

function upstreamRequest(url, key, body, accept) {
  const transport = url.protocol === 'https:' ? httpsRequest : httpRequest;
  return new Promise((resolveResponse, reject) => {
    const request = transport(url, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${key}`,
        accept,
        'accept-encoding': 'identity',
        'content-type': 'application/json',
        'content-length': Buffer.byteLength(body),
      },
      timeout: 300_000,
    }, resolveResponse);
    request.once('timeout', () => request.destroy(new Error('timeout')));
    request.once('error', reject);
    request.end(body);
  });
}

function safeStatus(status) {
  return Number.isInteger(status) && status >= 100 && status <= 599 ? status : 0;
}

function streamLlm(upstream, res, audit, kind) {
  const status = upstream.statusCode ?? 0;
  if (status >= 300 || status < 200) {
    upstream.resume();
    safeResponse(res, 502, status === 400 || status === 422 ? 'UI_PROXY_STREAM_FAILURE_MAPPED' : 'UI_PROXY_UPSTREAM_STATUS');
    appendFsync(audit, { event: 'upstream_status', kind, status: safeStatus(status), mapped: status === 400 || status === 422 });
    return;
  }
  const contentType = String(upstream.headers['content-type'] || '').toLowerCase();
  if (!contentType.includes('text/event-stream')) {
    upstream.resume();
    safeResponse(res, 502, 'UI_PROXY_UPSTREAM_STREAM_REQUIRED');
    appendFsync(audit, { event: 'upstream_content_type_rejected', kind });
    return;
  }
  res.statusCode = 200;
  res.setHeader('content-type', 'text/event-stream; charset=utf-8');
  res.setHeader('cache-control', 'no-store');
  res.flushHeaders();
  let aborted = false;
  res.once('close', () => {
    if (!res.writableEnded) {
      aborted = true;
      appendFsync(audit, { event: 'downstream_aborted_after_forward', kind });
      upstream.destroy();
    }
  });
  upstream.on('data', (chunk) => {
    if (!aborted && !res.write(chunk)) upstream.pause();
  });
  res.on('drain', () => upstream.resume());
  upstream.once('end', () => { if (!aborted) res.end(); });
  upstream.once('error', () => {
    appendFsync(audit, { event: 'upstream_stream_error_after_forward', kind });
    if (!res.writableEnded) res.destroy();
  });
}

function collectImage(upstream) {
  return new Promise((resolveImage, reject) => {
    const status = upstream.statusCode ?? 0;
    const chunks = [];
    let bytes = 0;
    upstream.on('data', (chunk) => {
      bytes += chunk.length;
      if (bytes > MAX_IMAGE_RESPONSE_BYTES) {
        upstream.destroy();
        reject(fail('UI_PROXY_IMAGE_RESPONSE_TOO_LARGE'));
      } else chunks.push(chunk);
    });
    upstream.once('error', () => reject(fail('UI_PROXY_UPSTREAM_TRANSPORT')));
    upstream.once('end', () => resolveImage({ status, bytes: Buffer.concat(chunks) }));
  });
}

export class NormalUiAcceptanceProxy {
  constructor(options) {
    this.options = {
      pythonPath: 'python',
      dbGateTimeoutMs: 5_000,
      ...options,
    };
    if (!this.options.runtimeRoot || !this.options.databasePath || !this.options.auditPath
      || !this.options.scopePath || !this.options.checkpointPath || !this.options.releasePath
      || !this.options.token || !this.options.realEnvPath) throw fail('UI_PROXY_CONFIG_REQUIRED');
    this.scopeDocument = null;
    this.scopeDigest = null;
    this.baseScope = null;
    this.provider = readRealProviderEnv(this.options.realEnvPath);
    this.server = null;
    this.reservedLock = Promise.resolve();
    this.bindingPath = resolve(this.options.runtimeRoot, 'scope-binding.json');
  }

  refreshScope() {
    const current = readJsonFile(this.options.scopePath, 'UI_PROXY_SCOPE_INVALID');
    const digest = sha256(JSON.stringify(current));
    if (this.scopeDigest !== null && digest !== this.scopeDigest) throw fail('UI_PROXY_SCOPE_CHANGED');
    const scope = verifyScope(current);
    this.scopeDocument = current;
    this.scopeDigest = digest;
    this.baseScope = scope;
    return scope;
  }

  async waitForScope(req, res) {
    const deadline = Date.now() + (this.options.scopeWaitMs ?? 30_000);
    while (Date.now() < deadline) {
      if (req.aborted || res.destroyed) throw fail('UI_PROXY_DOWNSTREAM_ABORTED', 499);
      if (existsSync(this.options.scopePath)) return this.refreshScope();
      await new Promise((resolveWait) => setTimeout(resolveWait, 500));
    }
    throw fail('UI_PROXY_SCOPE_REGISTRATION_TIMEOUT');
  }

  events() { return readEvents(this.options.auditPath); }

  reserve(kind, safeScope) {
    this.reservedLock = this.reservedLock.catch(() => undefined).then(() => {
      if (!KINDS.has(kind) || oneTimeUsed(this.events(), kind)) throw fail('UI_PROXY_BUDGET_EXHAUSTED');
      const reservation = {
        event: 'generation_post_reserved', kind, scopeDigest: sha256(JSON.stringify(safeScope)),
      };
      if (kind === 'source') {
        reservation.jobId = safeScope.jobId;
        reservation.sourceAnalysisRunId = safeScope.sourceAnalysisRunId;
        reservation.jobAttemptNo = safeScope.jobAttemptNo;
        reservation.sourceAttemptNo = safeScope.sourceAttemptNo;
      } else if (kind === 'adaptation') {
        reservation.jobId = safeScope.jobId;
        reservation.sourceAnalysisRunId = safeScope.sourceAnalysisRunId;
        reservation.adaptationAnalysisRunId = safeScope.adaptationAnalysisRunId;
        reservation.jobAttemptNo = safeScope.jobAttemptNo;
        reservation.adaptationAttemptNo = safeScope.adaptationAttemptNo;
      }
      appendFsync(this.options.auditPath, reservation);
    });
    return this.reservedLock;
  }

  bindSource(result) {
    if (!this.baseScope
      || result.jobId !== this.baseScope.productionJobId
      || result.sourceAnalysisRunId !== this.baseScope.sourceAnalysisRunId) {
      throw fail('UI_PROXY_SOURCE_BINDING_SCOPE_MISMATCH');
    }
    const binding = { schemaVersion: 'normal-ui-proxy-binding.v1', ...result };
    atomicJson(this.bindingPath, binding);
    appendFsync(this.options.auditPath, { event: 'source_scope_bound', jobId: result.jobId, sourceAnalysisRunId: result.sourceAnalysisRunId });
  }

  sourceBinding() {
    const binding = readJsonFile(this.bindingPath, 'UI_PROXY_SOURCE_BINDING_MISSING');
    if (binding.schemaVersion !== 'normal-ui-proxy-binding.v1'
      || typeof binding.jobId !== 'string' || typeof binding.sourceAnalysisRunId !== 'string'
      || typeof binding.comicAdaptationId !== 'string'
      || !Number.isInteger(binding.jobAttemptNo) || binding.jobAttemptNo < 1
      || !Number.isInteger(binding.sourceAttemptNo) || binding.sourceAttemptNo < 1) {
      throw fail('UI_PROXY_SOURCE_BINDING_MISSING');
    }
    return binding;
  }

  checkRelease(snapshot) {
    const release = readJsonFile(this.options.releasePath, 'UI_PROXY_IMAGE_RELEASE_REQUIRED');
    const expected = {
      jobId: snapshot.jobId,
      manifestId: snapshot.manifestId,
      manifestFingerprint: snapshot.manifestFingerprint,
      requestDigest: snapshot.requestDigest,
    };
    if (release.schemaVersion !== 'normal-ui-image-release.v1'
      || Object.entries(expected).some(([key, value]) => release[key] !== value)) {
      throw fail('UI_PROXY_IMAGE_RELEASE_SCOPE_MISMATCH');
    }
    if (this.events().some((event) => event.event === 'image_release_consumed')) {
      throw fail('UI_PROXY_IMAGE_RELEASE_ALREADY_USED');
    }
  }

  async handle(req, res) {
    try {
      if (!isLoopback(req.socket.remoteAddress)) throw fail('UI_PROXY_LOOPBACK_ONLY', 403);
      if (req.method !== 'POST') throw fail('UI_PROXY_METHOD_REJECTED', 405);
      if (!exactToken(req.headers.authorization, this.options.token)) throw fail('UI_PROXY_TOKEN_REJECTED', 401);
      const path = new URL(req.url, 'http://localhost').pathname;
      const contentType = String(req.headers['content-type'] || '').toLowerCase();
      if (!contentType.startsWith('application/json')) throw fail('UI_PROXY_JSON_REQUIRED', 415);
      const raw = await readRequestBody(req);
      const body = safeJsonParse(raw);
      if (path === '/v1/chat/completions') {
        await this.handleLlm(body, req, res);
      } else if (path === '/v1/images/generations') {
        await this.handleImage(body, req, res);
      } else {
        throw fail('UI_PROXY_PATH_REJECTED', 404);
      }
    } catch (error) {
      safeResponse(res, error?.status || 502, error?.message || 'UI_PROXY_INTERNAL');
    }
  }

  async handleLlm(body, req, res) {
    const baseScope = await this.waitForScope(req, res);
    if (body.model !== this.provider.llmModel) throw fail('UI_PROXY_LLM_MODEL_MISMATCH');
    const classified = classifyLlm(body, baseScope);
    let gateScope;
    if (classified.kind === 'source') {
      gateScope = classified.scope;
    } else {
      const binding = this.sourceBinding();
      if (classified.scope.comicAdaptationId !== binding.comicAdaptationId
        || binding.jobId !== baseScope.productionJobId
        || binding.sourceAnalysisRunId !== baseScope.sourceAnalysisRunId) {
        throw fail('UI_PROXY_ADAPTATION_OWNER_SCOPE_MISMATCH');
      }
      gateScope = { ...classified.scope, productionJobId: binding.jobId, sourceAnalysisRunId: binding.sourceAnalysisRunId };
    }
    const gate = runDbGate(this.options, classified.kind, gateScope);
    if (classified.kind === 'source') this.bindSource(gate);
    await this.reserve(classified.kind, gate);
    let upstream;
    try {
      upstream = await upstreamRequest(this.provider.llmUrl, this.provider.llmKey, JSON.stringify(body), 'text/event-stream');
      appendFsync(this.options.auditPath, { event: 'generation_http_received', kind: classified.kind, status: safeStatus(upstream.statusCode) });
    } catch {
      appendFsync(this.options.auditPath, { event: 'upstream_transport_error_after_forward', kind: classified.kind });
      throw fail('UI_PROXY_UPSTREAM_TRANSPORT');
    }
    if (req.aborted || res.destroyed) {
      appendFsync(this.options.auditPath, { event: 'downstream_aborted_after_forward', kind: classified.kind });
      upstream.destroy();
      return;
    }
    streamLlm(upstream, res, this.options.auditPath, classified.kind);
  }

  async waitForRelease(req, res, snapshot) {
    const reviewWindowMs = this.options.imageReviewWaitMs ?? 180_000;
    const { expires } = reviewWindow(snapshot, reviewWindowMs);
    while (Date.now() < expires) {
      if (req.aborted || res.destroyed) throw fail('UI_PROXY_DOWNSTREAM_ABORTED', 499);
      if (existsSync(this.options.releasePath)) {
        this.checkRelease(snapshot);
        return;
      }
      await new Promise((resolveWait) => setTimeout(resolveWait, 500));
    }
    throw fail('UI_PROXY_IMAGE_REVIEW_TIMEOUT');
  }

  async handleImage(body, req, res) {
    const baseScope = await this.waitForScope(req, res);
    validateImageBody(body);
    if (body.model !== this.provider.imageModel) throw fail('UI_PROXY_IMAGE_MODEL_MISMATCH');
    const binding = this.sourceBinding();
    if (binding.jobId !== baseScope.productionJobId || binding.sourceAnalysisRunId !== baseScope.sourceAnalysisRunId) {
      throw fail('UI_PROXY_IMAGE_SOURCE_BINDING_SCOPE_MISMATCH');
    }
    const gateScope = { ...baseScope };
    const gate = runDbGate(this.options, 'image', gateScope);
    const stableSnapshot = briefImageSnapshot(gate, body);
    let snapshot;
    if (!existsSync(this.options.checkpointPath)) {
      const created = Date.now();
      snapshot = {
        ...stableSnapshot,
        checkpointCreatedAtUtc: new Date(created).toISOString(),
        reviewExpiresAtUtc: new Date(created + (this.options.imageReviewWaitMs ?? 180_000)).toISOString(),
      };
      reviewWindow(snapshot, this.options.imageReviewWaitMs ?? 180_000);
      atomicJson(this.options.checkpointPath, snapshot);
      appendFsync(this.options.auditPath, { event: 'image_review_checkpoint_created', ...snapshot });
      process.stdout.write(`${JSON.stringify({ event: 'image_review_checkpoint_created', jobId: snapshot.jobId, manifestId: snapshot.manifestId, manifestFingerprint: snapshot.manifestFingerprint, requestDigest: snapshot.requestDigest, checkpointCreatedAtUtc: snapshot.checkpointCreatedAtUtc, reviewExpiresAtUtc: snapshot.reviewExpiresAtUtc })}\n`);
      await this.waitForRelease(req, res, snapshot);
    } else {
      const stored = readJsonFile(this.options.checkpointPath, 'UI_PROXY_IMAGE_CHECKPOINT_INVALID');
      reviewWindow(stored, this.options.imageReviewWaitMs ?? 180_000);
      if (JSON.stringify(stableImageReviewSnapshot(stored)) !== JSON.stringify(stableSnapshot)) throw fail('UI_PROXY_IMAGE_CHECKPOINT_SCOPE_MISMATCH');
      snapshot = stored;
      await this.waitForRelease(req, res, snapshot);
    }
    // Repeat the RO-BEGIN gate immediately before the irreversible reservation.
    const replayGate = runDbGate(this.options, 'image', gateScope);
    if (replayGate.manifestId !== gate.manifestId || replayGate.manifestFingerprint !== gate.manifestFingerprint) {
      throw fail('UI_PROXY_IMAGE_SCOPE_CHANGED');
    }
    await this.reserve('image', replayGate);
    appendFsync(this.options.auditPath, { event: 'image_release_consumed', jobId: snapshot.jobId, manifestId: snapshot.manifestId, requestDigest: snapshot.requestDigest });
    let upstream;
    try {
      upstream = await upstreamRequest(this.provider.imageUrl, this.provider.imageKey, JSON.stringify(body), 'application/json');
      appendFsync(this.options.auditPath, { event: 'generation_http_received', kind: 'image', status: safeStatus(upstream.statusCode) });
    } catch {
      appendFsync(this.options.auditPath, { event: 'upstream_transport_error_after_forward', kind: 'image' });
      throw fail('UI_PROXY_UPSTREAM_TRANSPORT');
    }
    const response = await collectImage(upstream);
    if (response.status < 200 || response.status >= 300) throw fail('UI_PROXY_UPSTREAM_STATUS');
    let value;
    try { value = JSON.parse(response.bytes.toString('utf8')); } catch { throw fail('UI_PROXY_IMAGE_RESPONSE_INVALID'); }
    const data = value?.data;
    if (!Array.isArray(data) || data.length !== 1 || typeof data[0]?.b64_json !== 'string' || data[0].b64_json.length === 0 || Object.hasOwn(data[0], 'url')) {
      throw fail('UI_PROXY_IMAGE_INLINE_REQUIRED');
    }
    res.statusCode = 200;
    res.setHeader('content-type', 'application/json; charset=utf-8');
    res.setHeader('cache-control', 'no-store');
    res.end(JSON.stringify({ data: [{ b64_json: data[0].b64_json }] }));
  }

  async listen() {
    if (this.server) throw fail('UI_PROXY_ALREADY_LISTENING');
    this.server = createServer((req, res) => { void this.handle(req, res); });
    await new Promise((resolveListen, reject) => {
      this.server.once('error', reject);
      this.server.listen(this.options.port ?? 0, '127.0.0.1', resolveListen);
    });
    const address = this.server.address();
    assert(address && typeof address === 'object');
    return address.port;
  }

  async close() {
    if (!this.server) return;
    await new Promise((resolveClose) => this.server.close(resolveClose));
    this.server = null;
  }
}

function parseArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index];
    if (!flag?.startsWith('--') || args[index + 1] === undefined) throw fail('UI_PROXY_ARGUMENTS_INVALID');
    options[flag.slice(2)] = args[index + 1];
  }
  return options;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const config = readJsonFile(args.config, 'UI_PROXY_CONFIG_INVALID');
  const proxy = new NormalUiAcceptanceProxy({
    ...config,
    runtimeRoot: config.runtimeRoot,
    databasePath: config.databasePath,
    auditPath: config.auditPath,
    scopePath: config.scopePath,
    checkpointPath: config.checkpointPath,
    releasePath: config.releasePath,
    realEnvPath: args['real-env'],
    token: process.env.UI_PROXY_TOKEN,
    port: Number(args.port),
  });
  const port = await proxy.listen();
  atomicJson(config.readyPath, { schemaVersion: PROXY_CONTRACT, pid: process.pid, port });
  const stop = async () => { await proxy.close(); process.exit(0); };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error) => { process.stderr.write(`${safeCode(error?.message)}\n`); process.exit(1); });
}
