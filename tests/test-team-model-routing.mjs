import assert from 'node:assert/strict';
import { apply } from '../node_modules/@deepseek-ai/dsh-experimental-tool-agent-team/lib/index.js';

const registered = [];
const calls = [];
let active = 0;
let maxActive = 0;
const lead = {
  session: { header: { origin: undefined } },
  ctx: {
    systemPrompt: {
      getSectionOrder: () => 0,
      section: () => () => {}
    },
    tools: {
      register: (tool) => {
        registered.push(tool);
        return () => {};
      }
    }
  }
};

const ctx = {
  agentTeams: {
    tryMembership: () => ({ role: 'lead', root: lead }),
    spawnTeammate: async (_agent, request) => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      calls.push(request);
      await new Promise((resolve) => setTimeout(resolve, 20));
      active -= 1;
      return { member: { id: 'member-1', name: 'developer', role: 'teammate', status: 'provisioning', diagnostics: [] } };
    }
  },
  agents: { list: () => [lead] },
  on: () => {},
  effect: () => {}
};

apply(ctx);
const spawn = registered.find((tool) => tool.name === 'spawn_teammate');
assert.ok(spawn, 'spawn_teammate must be registered');

await spawn.execute({
  name: 'developer',
  description: 'implementation',
  prompt: 'implement the task',
  context: 'fresh',
  llm_provider: 'peachsh-key-2',
  model: 'deepseek-v4.1-flash',
  reasoning_effort: 'medium',
  max_tokens: 2048
}, { agent: lead, signal: new AbortController().signal });

await Promise.all([
  spawn.execute({
    name: 'reviewer', description: 'review', prompt: 'review the task', context: 'fresh',
    llm_provider: 'peachsh-key-3', model: 'deepseek-v4.1-flash'
  }, { agent: lead, signal: new AbortController().signal }),
  spawn.execute({
    name: 'tester', description: 'test', prompt: 'test the task', context: 'fresh',
    llm_provider: 'peachsh-key-1', model: 'deepseek-v4.1-flash'
  }, { agent: lead, signal: new AbortController().signal })
]);

assert.equal(calls.length, 3);
assert.equal(calls[0].provider, 'spawn', 'Team execution provider stays separate');
assert.deepEqual(calls[0].agentOptions, {
  provider: 'peachsh-key-2',
  model: 'deepseek-v4.1-flash',
  reasoningEffort: 'medium',
  maxTokens: 2048
});
assert.match(calls[0].prompt[0].text, /named "developer"/u);
assert.match(calls[0].prompt[0].text, /role is "teammate"/u);
assert.match(calls[0].prompt[0].text, /responsibility is "implementation"/u);
assert.equal(maxActive, 2, 'independent teammates can be admitted concurrently');
assert.deepEqual(calls.slice(1).map((request) => request.agentOptions.provider), ['peachsh-key-3', 'peachsh-key-1']);

console.log('team model routing: ok');
