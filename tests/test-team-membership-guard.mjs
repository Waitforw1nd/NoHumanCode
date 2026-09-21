import assert from 'node:assert/strict';
import { apply } from '../node_modules/@deepseek-ai/dsh-experimental-tool-agent-team/lib/index.js';

const rootTools = [];
const provisionalTools = [];
const root = {
  id: 'root',
  session: { header: { origin: undefined } },
  ctx: {
    systemPrompt: { getSectionOrder: () => 0, section: () => () => {} },
    tools: { register: (tool) => { rootTools.push(tool); return () => {}; } }
  }
};
const provisional = {
  id: 'ordinary-child',
  session: { header: { origin: 'subagent' } },
  ctx: {
    systemPrompt: { getSectionOrder: () => 0, section: () => () => {} },
    tools: { register: (tool) => { provisionalTools.push(tool); return () => {}; } }
  }
};

apply({
  agentTeams: { tryMembership: () => ({ role: 'lead', root }) },
  agents: { list: () => [root, provisional] },
  on: () => {},
  effect: () => {}
});

assert.ok(rootTools.some((tool) => tool.name === 'spawn_teammate'));
assert.equal(provisionalTools.length, 0, 'ordinary foreground subagents must not receive Team tools');
console.log('team membership guard: ok');
