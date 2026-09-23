import assert from 'node:assert/strict';

const baseURL = (process.env.PEACHSH_TEST_BASE_URL ?? 'https://xpeach.codes/v1').replace(/\/$/u, '');
const model = process.env.PEACHSH_TEST_MODEL ?? 'gpt-5.6-sol';
const keys = [process.env.PEACHSH_TEST_KEY_1, process.env.PEACHSH_TEST_KEY_2];
if (keys.some((key) => !key)) {
  throw new Error('Set PEACHSH_TEST_KEY_1 and PEACHSH_TEST_KEY_2 for the live two-key check. The values are never printed.');
}

const started = performance.now();
const results = await Promise.all(keys.map(async (key, index) => {
  const response = await fetch(`${baseURL}/chat/completions`, {
    method: 'POST',
    headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json' },
    body: JSON.stringify({
      model,
      messages: [{ role: 'user', content: `Reply exactly PEACH${index + 1}` }],
      max_tokens: 8,
      stream: false
    })
  });
  return { status: response.status, elapsedMs: Math.round(performance.now() - started) };
}));

assert.ok(results.every(({ status }) => status >= 200 && status < 300), `provider rejected one or more requests: ${results.map(({ status }) => status).join(',')}`);
console.log(JSON.stringify({ baseURL, model, requests: results.length, results, parallel: true }));
