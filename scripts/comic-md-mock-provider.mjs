import http from 'node:http';
import { writeFile } from 'node:fs/promises';
import { pngBase64 } from './comic-md-fixtures.mjs';

const [portText, auditRoot] = process.argv.slice(2);
const port = Number(portText);
if (!port || !auditRoot) throw new Error('Usage: node comic-md-mock-provider.mjs PORT AUDIT_ROOT');
const requests = [];
const queue = [];
const heldTexts = [];
let imageCalls = 0;
let failImageAt = null;
let holdImages = false;
const persist = () => writeFile(`${auditRoot}/mock-requests.json`, JSON.stringify({ requests, queued: queue.length, imageCalls }, null, 2));
const server = http.createServer(async (req, res) => {
  try {
    let raw = '';
    for await (const chunk of req) raw += chunk;
    const body = raw ? JSON.parse(raw) : {};
    const json = (status, value) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); };
    if (req.url === '/__control' && req.method === 'POST') {
      if (body.clearTextQueue) queue.splice(0);
      if (typeof body.markdown === 'string') queue.push({ markdown: body.markdown, mode: body.textMode ?? 'complete', hold: body.holdText ?? false });
      if (body.releaseText) { for (const release of heldTexts.splice(0)) release(); }
      if ('failImageAt' in body) failImageAt = body.failImageAt;
      if ('holdImages' in body) holdImages = body.holdImages;
      return json(200, { imageCalls, requests: requests.length });
    }
    if (req.url === '/__state') return json(200, { requests, imageCalls, queued: queue.length, heldTexts: heldTexts.length });
    if (req.url === '/v1/chat/completions' && req.method === 'POST') {
      requests.push({ kind: 'text', body, at: new Date().toISOString() });
      await persist();
      const fixture = queue.shift();
      if (fixture === undefined) return json(409, { error: { message: 'No text fixture queued; unexpected model call' } });
      const { markdown, mode } = fixture;
      if (fixture.hold) await new Promise(resolve => heldTexts.push(resolve));
      if (mode === 'nonstream_length') return json(200, { choices: [{ message: { role: 'assistant', content: markdown }, finish_reason: 'length' }] });
      if (body.stream) {
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        const contentFrame = `data: ${JSON.stringify({ choices: [{ delta: { content: markdown }, finish_reason: null }] })}\n\n`;
        res.end(mode === 'early_eof' ? contentFrame : `${contentFrame}data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: mode === 'sse_length' ? 'length' : 'stop' }] })}\n\ndata: [DONE]\n\n`);
      } else json(200, { choices: [{ message: { role: 'assistant', content: markdown }, finish_reason: 'stop' }] });
      return;
    }
    if (req.url === '/v1/images/generations' && req.method === 'POST') {
      imageCalls++;
      requests.push({ kind: 'image', body, at: new Date().toISOString() });
      await persist();
      if (holdImages) return; // supervisor intentionally kills the isolated app for recovery verification
      if (imageCalls === failImageAt) return json(400, { error: { message: 'Intentional local fixture failure' } });
      return json(200, { data: [{ b64_json: pngBase64 }] });
    }
    requests.push({ kind: 'unexpected', method: req.method, url: req.url });
    await persist();
    json(404, { error: 'Unexpected mock route' });
  } catch (error) {
    res.writeHead(500); res.end(String(error));
  }
});
server.listen(port, '127.0.0.1', () => console.log(`Local mock listening at 127.0.0.1:${port}`));
