import process from 'node:process';

const baseURL = (process.env.PEACHSH_TEST_BASE_URL ?? 'https://xpeach.codes/v1').replace(/\/$/u, '');
const key = process.env.PEACHSH_TEST_KEY;
if (!key) throw new Error('Set PEACHSH_TEST_KEY for the live xpeach check. The value is never printed.');

const headers = { authorization: `Bearer ${key}`, 'content-type': 'application/json' };
const started = performance.now();
const modelResponse = await fetch(`${baseURL}/models`, { headers: { authorization: `Bearer ${key}` } });
let modelPayload = null;
try { modelPayload = await modelResponse.json(); } catch { modelPayload = null; }
const advertised = Array.isArray(modelPayload?.data)
  ? modelPayload.data.map((entry) => typeof entry === 'string' ? entry : entry?.id).filter(Boolean)
  : Array.isArray(modelPayload?.models) ? modelPayload.models.map((entry) => typeof entry === 'string' ? entry : entry?.id).filter(Boolean) : [];
const normalize = (value) => String(value).toLowerCase().replace(/[^a-z0-9]/gu, '');
const desired = (process.env.PEACHSH_TEST_MODELS ?? 'gpt5.6sol,gpt6').split(',').map((value) => value.trim()).filter(Boolean);
const models = desired.map((wanted) => {
  const wantedKey = normalize(wanted);
  const exact = advertised.find((candidate) => normalize(candidate) === wantedKey);
  if (exact) return exact;
  // xpeach advertises the GPT 6 route as gpt-6-astra while users commonly
  // refer to it as gpt6; keep the short alias convenient for smoke tests.
  if (wantedKey === 'gpt6') return advertised.find((candidate) => normalize(candidate).startsWith('gpt6')) ?? wanted;
  return wanted;
});
if (!modelResponse.ok) throw new Error(`model listing failed with HTTP ${modelResponse.status}`);
if (models.length === 0) throw new Error('no test models configured');

async function runSmallTask(model, index) {
  const response = await fetch(`${baseURL}/chat/completions`, {
    method: 'POST',
    headers,
    body: JSON.stringify({
      model,
      messages: [{ role: 'user', content: `Return only the number for ${17 + index} * ${19 + index}.` }],
      max_tokens: 16,
      stream: true
    })
  });
  const raw = await response.text();
  let payload = null;
  try { payload = JSON.parse(raw); } catch { payload = null; }
  if (!response.ok) return { model, status: response.status, ok: false, error: typeof payload?.error?.message === 'string' ? payload.error.message.slice(0, 180) : raw.slice(0, 180) || 'request failed' };
  const content = raw.split(/\r?\n/u)
    .filter((line) => line.startsWith('data:'))
    .map((line) => line.slice(5).trim())
    .filter((line) => line && line !== '[DONE]')
    .map((line) => { try { return JSON.parse(line); } catch { return null; } })
    .filter(Boolean)
    .map((chunk) => chunk?.choices?.[0]?.delta?.content ?? '')
    .join('');
  return { model, status: response.status, ok: true, answer: content.trim().slice(0, 80) || null };
}

const results = await Promise.all(models.map(runSmallTask));
const output = {
  baseURL,
  modelListing: { status: modelResponse.status, advertisedCount: advertised.length, advertisedModels: advertised, selected: models },
  requests: results,
  parallel: true,
  elapsedMs: Math.round(performance.now() - started)
};
console.log(JSON.stringify(output, null, 2));
if (!results.every((result) => result.ok)) process.exitCode = 1;
