import assert from 'node:assert/strict';
import { normalizeBalance, queryBalance } from '../data/skills/newapi/scripts/query-newapi-balance.mjs';

const normalized = normalizeBalance({
  data: {
    username: 'demo', display_name: 'Demo User', email: 'demo@example.invalid',
    quota: 1000000, used_quota: 250000, group: 'pro', request_count: 7,
  },
}, { baseURL: 'http://localhost:8788', userId: '42', checkedAt: '2026-01-01T00:00:00.000Z' });
assert.equal(normalized.quota, 2);
assert.equal(normalized.usedQuota, 0.5);
assert.equal(normalized.username, 'demo');
assert.equal(normalized.requestCount, 7);

let seen;
const result = await queryBalance({ baseURL: 'http://localhost:8788', userId: '42', token: 'test-token' }, async (url, options) => {
  seen = { url, options };
  return new Response(JSON.stringify({ quota: 500000, used_quota: 125000, username: 'demo' }), { status: 200, headers: { 'content-type': 'application/json' } });
});
assert.equal(seen.url, 'http://localhost:8788/api/user/self');
assert.equal(seen.options.headers.Authorization, 'Bearer test-token');
assert.equal(seen.options.headers['New-Api-User'], '42');
assert.equal(result.quota, 1);
assert.equal(result.usedQuota, 0.25);

await assert.rejects(
  () => queryBalance({ baseURL: 'http://localhost:8788', userId: '42', token: 'secret-token' }, async () =>
    new Response(JSON.stringify({ message: 'Bearer secret-token rejected' }), { status: 401 })),
  (error) => error.message === 'New API returned HTTP 401: Bearer [redacted] rejected',
);
console.log('newapi balance: ok');
