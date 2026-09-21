import assert from 'node:assert/strict';
import { FileSystemSkillProvider } from '../node_modules/@deepseek-ai/dsh-skill-filesystem/lib/index.js';

const controller = new AbortController();
const provider = new FileSystemSkillProvider(
  { logger: { warn() {}, error() {} }, get() { return undefined; } },
  { invalidate() {}, signal: controller.signal },
  { dshHome: 'D:/peachsh-harness/data', agentsHome: 'D:/peachsh-harness/data/agents', watch: false },
);
const candidates = await provider.list('D:/peachsh-harness');
const names = candidates.map((entry) => entry.name).sort();
assert.ok(names.includes('newapi'), 'newapi skill must be discoverable');
assert.ok(names.includes('swarm'), 'swarm skill must be discoverable');
const loaded = await provider.get(candidates.find((entry) => entry.name === 'swarm'), { signal: controller.signal });
assert.equal(loaded.name, 'swarm');
assert.equal(loaded.invocation.userInvocable, true);
await provider.dispose();
console.log('skill discovery: ok');
