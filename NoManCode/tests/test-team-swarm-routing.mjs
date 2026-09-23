import assert from 'node:assert/strict';
import { apply } from '../node_modules/@deepseek-ai/dsh-experimental-tool-agent-team/lib/index.js';

const registered = [];
const calls = [];
const messages = [];
const members = [];
let active = 0;
let maxActive = 0;
const lead = {
  id: 'lead',
  session: { header: { origin: undefined } },
  ctx: {
    systemPrompt: { getSectionOrder: () => 0, section: () => () => {} },
    tools: { register: (tool) => { registered.push(tool); return () => {}; } }
  }
};

const ctx = {
  agentTeams: {
    tryMembership: () => ({ role: 'lead', root: lead }),
    spawnTeammate: async (_agent, request) => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      calls.push(request);
      const member = { id: `member-${calls.length}`, name: request.name, role: 'teammate', status: 'running', diagnostics: [] };
      members.push(member);
      await new Promise((resolve) => setTimeout(resolve, 20));
      member.status = 'idle';
      active -= 1;
      return { member };
    },
    listMembers: () => [
      { id: 'lead', name: 'lead', role: 'lead', status: 'running', diagnostics: [] },
      ...members
    ],
    waitForChange: async () => ({ timedOut: false }),
    sendMessage: async (_agent, request) => {
      messages.push(request);
      return { messageId: `message-${messages.length}`, status: 'accepted' };
    }
  },
  agents: { list: () => [lead] },
  on: () => {},
  effect: () => {}
};

apply(ctx);
const start = registered.find((tool) => tool.name === 'swarm_start');
const wait = registered.find((tool) => tool.name === 'swarm_wait');
const resume = registered.find((tool) => tool.name === 'swarm_resume');
assert.ok(start && wait && resume, 'Swarm tools must be registered');

const started = await start.execute({
  max_concurrency: 2,
  shared_instructions: 'Keep each patch small and report verification.',
  items: [
    { name: 'planner', role: 'planner', task: 'outline the implementation', llm_provider: 'peachsh-key-1', model: 'gpt-5.6-sol' },
    { name: 'developer', role: 'developer', task: 'implement the core module', llm_provider: 'peachsh-key-2', model: 'deepseek-v4.1-flash', write_scopes: ['src/core'] },
    { name: 'reviewer', role: 'reviewer', task: 'review the patch', llm_provider: 'peachsh-key-3', model: 'gpt-5.6-sol', write_scopes: [] }
  ]
}, { agent: lead, signal: new AbortController().signal });

assert.equal(started.accepted.length, 3);
assert.equal(started.rejected.length, 0);
assert.equal(maxActive, 2);
assert.deepEqual(calls.map((request) => request.agentOptions.model), ['gpt-5.6-sol', 'deepseek-v4.1-flash', 'gpt-5.6-sol']);
assert.deepEqual(calls.map((request) => request.agentOptions.provider), ['peachsh-key-1', 'peachsh-key-2', 'peachsh-key-3']);
assert.match(calls[1].prompt[0].text, /Write scope: src\/core/u);

const waited = await wait.execute({ run_id: started.run_id, timeout_ms: 10000 }, { agent: lead, signal: new AbortController().signal });
assert.equal(waited.timed_out, false);
assert.equal(waited.members.length, 3);
await resume.execute({ run_id: started.run_id, targets: [{ target: 'developer', message: 'Please rerun the focused test and report the result.' }] }, { agent: lead, signal: new AbortController().signal });
assert.equal(messages.length, 1);
await assert.rejects(
  () => start.execute({
    items: [
      { name: 'api-worker', task: 'edit api', write_scopes: ['src/api'] },
      { name: 'api-reviewer', task: 'review api', write_scopes: ['src/api/tests'] }
    ]
  }, { agent: lead, signal: new AbortController().signal }),
  /write scope overlaps/u,
);
console.log('team swarm routing: ok');
