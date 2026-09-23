import z from "@deepseek-ai/schemastery";
import { TeamTaskId } from "@deepseek-ai/dsh-experimental-agent-team";
import { defineTool } from "@deepseek-ai/dsh-tools";
//#region lib/types/index.js
/** Scoped model-facing tools for the opt-in Agent Teams runtime. */
/** Cordis plugin name. */
const name = "tool-agent-team";
/** Services required by the Team tool plugin. */
const inject = [
	"agents",
	"agentTeams",
	"tools",
	"systemPrompt"
];
/** Loader schema for the opt-in Team tool plugin. */
const Config = z.object({
	freshProvider: z.string().default("spawn"),
	forkProvider: z.string().default("fork"),
	swarmMaxConcurrency: z.number().default(3)
});
/** Model-facing collaboration guidance shared by Lead and teammates. */
const POLICY = `Agent Teams is available in this session, but create teammates only when the user explicitly asks to use Agent Teams or teammates.

🍑sh harness model routing: when the user asks for different models or Keys, pass the teammate's LLM route as 'llm_provider' (for example 'peachsh-key-1', 'peachsh-key-2', or 'peachsh-key-3') and pass its exact 'model'. This is separate from the Team execution provider selected by context. Do not put API keys in prompts, messages, or task descriptions. Spawn independent teammates without waiting for their work; use dependencies for ordered work and wait only after all required members have been started.

🍑sh harness Swarm mode: when the user asks for a swarm or a multi-model development run, prefer \`swarm_start\` over manually issuing many \`spawn_teammate\` calls. Give every item a unique lower-kebab-case name, an explicit responsibility, and a disjoint write scope. Select the route/model per item when different models are needed. \`swarm_start\` admits independent workers with a bounded concurrency cap and returns a run id; use \`swarm_wait\` once all workers are admitted, then use \`swarm_resume\` for a targeted follow-up. Each worker must send a concise result to the Lead and must not edit outside its declared scope.

The Team Lead and all teammates share the same working directory and filesystem. Edits are immediately visible to every member. Split write work into disjoint scopes, record expected write scopes on shared tasks, and use task dependencies when work must be ordered. Write-scope overlap is advisory, not a lock.

Prefer read/edit/write for file changes. If a file operation returns FS_STALE_VERSION, read the current file, rebase your intended change onto the new content, and retry. Bash, formatters, code generators, and scripts are not fully protected by the filesystem version guard; coordinate them explicitly and have the Lead review the final diff and run tests.

send_message steers a running target at its nearest step boundary, starts an idle target, and cold-resumes an inactive teammate. A delivered peer item starts with its stable message id and sender name. A successful send is already durable even when its result says queued; do not resend it. Shared-task workflow is list, get, claim with the current revision, perform the work, then complete. Task readiness never starts an owner. Before wait_agent, use list_agents and make sure another required member is running or provisioning; use send_message first when the required member is inactive. wait_agent observes only changes after that call starts, never wakes a member, and returns noProgress immediately when no other member can produce a change. Re-list after wakeup or timeout. The Lead must wait for required teammates before giving the final answer.`;
const ACTIVE_WAIT_STATUSES = new Set(["running", "provisioning"]);
const NO_ACTIVE_PEER_MESSAGE = "No other Team member is running or provisioning. wait_agent cannot make progress or wake inactive teammates. Re-list with list_agents and team_task_list, then use send_message to wake each required inactive teammate before waiting again.";
/**
* One roster row, matching `TeamMemberView`. The Lead pseudo-row omits the
* teammate-only provisioning fields, so only identity, role, status, and
* diagnostics are required.
*/
const MEMBER_VIEW_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		id: {
			type: "string",
			required: true
		},
		name: {
			type: "string",
			required: true
		},
		role: {
			type: "string",
			required: true,
			enum: ["lead", "teammate"]
		},
		status: {
			type: "string",
			required: true,
			enum: [
				"running",
				"idle",
				"inactive",
				"provisioning",
				"failed"
			]
		},
		description: { type: "string" },
		provider: { type: "string" },
		context: {
			type: "string",
			enum: ["fresh", "fork"]
		},
		llmProvider: { type: "string" },
		model: { type: "string" },
		diagnostics: {
			type: "array",
			required: true,
			items: { type: "string" }
		}
	}
};
/** One shared task, matching the public `TeamTaskView`. */
const TASK_VIEW_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		id: {
			type: "string",
			required: true
		},
		revision: {
			type: "integer",
			required: true
		},
		subject: {
			type: "string",
			required: true
		},
		description: {
			type: "string",
			required: true
		},
		status: {
			type: "string",
			required: true,
			enum: [
				"pending",
				"in_progress",
				"completed",
				"deleted"
			]
		},
		ownerName: { type: "string" },
		blockedBy: {
			type: "array",
			required: true,
			items: { type: "string" }
		},
		writeScopes: {
			type: "array",
			required: true,
			items: { type: "string" }
		},
		ready: {
			type: "boolean",
			required: true
		},
		writeScopeWarnings: {
			type: "array",
			required: true,
			items: { type: "string" }
		}
	}
};
const SPAWN_VALUE_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: { member: {
		...MEMBER_VIEW_SCHEMA,
		required: true
	} }
};
const MEMBER_LIST_VALUE_SCHEMA = {
	type: "array",
	items: MEMBER_VIEW_SCHEMA
};
const SEND_VALUE_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		messageId: {
			type: "string",
			required: true
		},
		status: {
			type: "string",
			required: true,
			enum: ["accepted", "queued"]
		}
	}
};
/** `noProgress` is present only on the model-only shortcut that skips the wait. */
const WAIT_VALUE_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		timedOut: {
			type: "boolean",
			required: true
		},
		noProgress: {
			type: "object",
			additionalProperties: false,
			properties: {
				reason: {
					type: "string",
					required: true,
					const: "no-active-peer"
				},
				message: {
					type: "string",
					required: true
				}
			}
		}
	}
};
const INTERRUPT_VALUE_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: { previousStatus: {
		type: "string",
		required: true,
		enum: [
			"running",
			"idle",
			"inactive"
		]
	} }
};
const TASK_LIST_VALUE_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		tasks: {
			type: "array",
			required: true,
			items: TASK_VIEW_SCHEMA
		},
		nextCursor: { type: "integer" }
	}
};
/** One independent item admitted by the multi-model Swarm launcher. */
const SWARM_ITEM_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		name: { type: "string", required: true },
		task: { type: "string", required: true },
		role: { type: "string" },
		description: { type: "string" },
		context: { type: "string", enum: ["fresh", "fork"] },
		llm_provider: { type: "string" },
		model: { type: "string" },
		reasoning_effort: { type: "string" },
		max_tokens: { type: "integer" },
		write_scopes: { type: "array", items: { type: "string" } }
	}
};
const SWARM_ACCEPTED_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		name: { type: "string", required: true },
		task: { type: "string", required: true },
		llmProvider: { type: "string" },
		model: { type: "string" },
		member: { ...MEMBER_VIEW_SCHEMA, required: true }
	}
};
const SWARM_REJECTED_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		name: { type: "string", required: true },
		task: { type: "string", required: true },
		error: { type: "string", required: true }
	}
};
const SWARM_RESULT_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		run_id: { type: "string", required: true },
		max_concurrency: { type: "integer", required: true },
		accepted: { type: "array", required: true, items: SWARM_ACCEPTED_SCHEMA },
		rejected: { type: "array", required: true, items: SWARM_REJECTED_SCHEMA }
	}
};
const SWARM_WAIT_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		run_id: { type: "string", required: true },
		timed_out: { type: "boolean", required: true },
		members: { type: "array", required: true, items: MEMBER_VIEW_SCHEMA }
	}
};
const SWARM_RESUME_SCHEMA = {
	type: "object",
	additionalProperties: false,
	properties: {
		accepted: {
			type: "array",
			required: true,
			items: {
				type: "object",
				additionalProperties: false,
				properties: {
					target: { type: "string", required: true },
					messageId: { type: "string", required: true },
					status: { type: "string", required: true, enum: ["accepted", "queued"] }
				}
			}
		}
	}
};
/**
* Declare one canonical output schema with compact model-facing JSON. Every
* Team result is a fixed record, so the declared schema is what makes the
* compiler check `execute` against the value the model is promised.
* @param schema - canonical value schema for one tool.
* @returns the `output` declaration accepted by {@link defineTool}.
*/
function jsonOutput(schema) {
	return {
		schema,
		render: (_args, value) => [{
			type: "text",
			text: JSON.stringify(value)
		}]
	};
}
/** Recover the exact caller guaranteed by Agent-scoped tool discovery. */
function callingAgent(agent, toolName) {
	/* v8 ignore next 2 -- Team tools are registered only in an exact Agent scope, so discovery supplies this carrier. */
	if (agent === void 0) throw new Error(`${toolName} requires a calling Agent`);
	return agent;
}
function safeSwarmError(error) {
	const text = error instanceof Error ? error.message : String(error);
	return text.replace(/(?:bearer|token|api[-_ ]?key|secret)[=: ]+\S+/giu, "$1=[redacted]").slice(0, 240);
}
function routeOptions(item, defaults) {
	const llmProvider = typeof item.llm_provider === "string" ? item.llm_provider.trim() : (defaults.llm_provider ?? "");
	const model = typeof item.model === "string" ? item.model.trim() : (defaults.model ?? "");
	const reasoningEffort = typeof item.reasoning_effort === "string" ? item.reasoning_effort.trim() : (defaults.reasoning_effort ?? "");
	const maxTokens = item.max_tokens ?? defaults.max_tokens;
	return llmProvider || model || reasoningEffort || maxTokens !== void 0 ? {
		...(llmProvider === "" ? {} : { provider: llmProvider }),
		...(model === "" ? {} : { model }),
		...(reasoningEffort === "" ? {} : { reasoningEffort }),
		...(maxTokens === void 0 ? {} : { maxTokens })
	} : void 0;
}
async function runWithConcurrency(items, limit, task) {
	const results = new Array(items.length);
	let next = 0;
	const worker = async () => {
		while (true) {
			const index = next++;
			if (index >= items.length) return;
			try {
				results[index] = await task(items[index], index);
			} catch (error) {
				results[index] = { error };
			}
		}
	};
	await Promise.all(Array.from({ length: Math.min(limit, items.length) }, () => worker()));
	return results;
}
function swarmPrompt({ runId, index, total, item, sharedInstructions }) {
	const role = typeof item.role === "string" && item.role.trim() !== "" ? item.role.trim() : "specialist";
	const description = typeof item.description === "string" && item.description.trim() !== "" ? item.description.trim() : role;
	const scopes = Array.isArray(item.write_scopes) && item.write_scopes.length > 0 ? item.write_scopes.join(", ") : "none declared; remain read-only unless the Lead expands the scope";
	return `<system-reminder>\nYou are a 🍑sh harness Swarm worker. Your stable name is "${item.name.trim()}" and your role is "${role}". Swarm run: ${runId}; item ${String(index + 1)}/${String(total)}. Responsibility: ${description}.\nWrite scope: ${scopes}. Do not edit outside this scope.\nWhen finished, send the Lead a concise result with: status, files changed, tests run, risks, and next action.\n</system-reminder>\n\n${sharedInstructions}\n\nTask:\n${item.task}`;
}
function swarmRunFor(caller, runId, swarmRuns, ctx) {
	const cached = swarmRuns.get(runId);
	if (cached !== void 0) return cached;
	const names = ctx.agentTeams.listMembers(caller).filter((member) => typeof member.description === "string" && member.description.includes(`[swarm:${runId}]`)).map((member) => member.name);
	if (names.length === 0) return void 0;
	const recovered = { names };
	swarmRuns.set(runId, recovered);
	return recovered;
}
/** Register the complete Team tool set in one exact Agent scope. */
function install(agent, ctx, config) {
	const scoped = agent.ctx;
	const disposers = [];
	const swarmRuns = new Map();
	const register = (disposer) => {
		disposers.push(disposer);
	};
	try {
		register(scoped.systemPrompt.section({
			name: "team:policy",
			order: scoped.systemPrompt.getSectionOrder("TEAM_POLICY"),
			text: POLICY
		}));
		register(scoped.tools.register(defineTool({
			name: "spawn_teammate",
			description: "Create one named, durable teammate. Only the Team Lead may call this tool.",
			parameters: {
				name: {
					type: "string",
					required: true,
					description: "Unique lower-kebab-case teammate name."
				},
				description: {
					type: "string",
					required: true,
					description: "Short description of the delegated responsibility."
				},
				prompt: {
					type: "string",
					required: true,
					description: "Complete initial task for the teammate."
				},
				context: {
					type: "string",
					enum: ["fresh", "fork"],
					description: "fresh starts without Lead history; fork inherits completed Lead turns. Defaults to fresh."
				},
				llm_provider: {
					type: "string",
					description: "Optional Harness LLM route alias for this teammate. Use a separate route when the teammate needs a different API key."
				},
				model: {
					type: "string",
					description: "Optional model id on the selected LLM route."
				},
				reasoning_effort: {
					type: "string",
					description: "Optional provider-supported reasoning effort."
				},
				max_tokens: {
					type: "integer",
					description: "Optional positive output-token cap for this teammate."
				}
			},
			output: jsonOutput(SPAWN_VALUE_SCHEMA),
			async execute(args, exec) {
				const agent = callingAgent(exec.agent, "spawn_teammate");
				const context = args.context ?? "fresh";
				const llmProvider = typeof args.llm_provider === "string" ? args.llm_provider.trim() : "";
				const model = typeof args.model === "string" ? args.model.trim() : "";
				const reasoningEffort = typeof args.reasoning_effort === "string" ? args.reasoning_effort.trim() : "";
				const agentOptions = llmProvider || model || reasoningEffort || args.max_tokens !== void 0 ? {
					...(llmProvider === "" ? {} : { provider: llmProvider }),
					...(model === "" ? {} : { model }),
					...(reasoningEffort === "" ? {} : { reasoningEffort }),
					...(args.max_tokens === void 0 ? {} : { maxTokens: args.max_tokens })
				} : void 0;
				return await ctx.agentTeams.spawnTeammate(agent, {
					name: args.name,
					description: args.description,
					prompt: [{
						type: "text",
						text: `<system-reminder>\nYou are the Agent Team teammate named "${args.name.trim()}". Your Team role is "teammate". Your responsibility is "${args.description.trim()}". The name is your stable identity; the responsibility is not a name or a role change.\n</system-reminder>\n\n`
					}, {
						type: "text",
						text: args.prompt
					}],
					context,
					provider: context === "fork" ? config.forkProvider : config.freshProvider,
					agentOptions,
					signal: exec.signal
				});
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "swarm_start",
			description: "Launch a bounded batch of durable Team workers, allowing each item to select its own LLM route and model. Use this for multi-model development work.",
			parameters: {
				items: {
					type: "array",
					required: true,
					items: SWARM_ITEM_SCHEMA,
					description: "At least two independent work items. Every item needs a unique lower-kebab-case name and a task."
				},
				shared_instructions: { type: "string", description: "Instructions shared by all workers." },
				default_context: { type: "string", enum: ["fresh", "fork"], description: "Default child context; fresh is recommended for independent tasks." },
				default_llm_provider: { type: "string", description: "Fallback Harness LLM route alias for items without llm_provider." },
				default_model: { type: "string", description: "Fallback model id for items without model." },
				default_reasoning_effort: { type: "string", description: "Fallback reasoning effort." },
				default_max_tokens: { type: "integer", description: "Fallback output-token cap." },
				max_concurrency: { type: "integer", description: "Maximum workers admitted at once; defaults to the configured Swarm cap." }
			},
			output: jsonOutput(SWARM_RESULT_SCHEMA),
			async execute(args, exec) {
				const caller = callingAgent(exec.agent, "swarm_start");
				const items = Array.isArray(args.items) ? args.items : [];
				if (items.length < 2) throw new Error("swarm_start requires at least 2 items");
				if (items.length > 32) throw new Error("swarm_start supports at most 32 items per run");
				const names = new Set();
				const scopes = [];
				for (const item of items) {
					if (typeof item.name !== "string" || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/u.test(item.name) || item.name === "lead") throw new Error(`invalid Swarm worker name: ${String(item.name)}`);
					if (names.has(item.name)) throw new Error(`duplicate Swarm worker name: ${item.name}`);
					names.add(item.name);
					if (item.max_tokens !== void 0 && (!Number.isSafeInteger(item.max_tokens) || item.max_tokens < 1)) throw new Error(`max_tokens must be positive for ${item.name}`);
					for (const rawScope of Array.isArray(item.write_scopes) ? item.write_scopes : []) {
						const scope = String(rawScope).trim().replaceAll("\\", "/").replace(/^\.\//u, "").replace(/\/+$/u, "");
						if (scope === "") throw new Error(`empty write scope for ${item.name}`);
						const conflict = scopes.find((entry) => entry.scope === scope || entry.scope.startsWith(`${scope}/`) || scope.startsWith(`${entry.scope}/`));
						if (conflict !== void 0) throw new Error(`Swarm write scope overlaps: ${item.name} (${scope}) and ${conflict.name} (${conflict.scope})`);
						scopes.push({ name: item.name, scope });
					}
				}
				const configuredLimit = Number.isSafeInteger(config.swarmMaxConcurrency) ? config.swarmMaxConcurrency : 3;
				const requestedLimit = args.max_concurrency ?? configuredLimit;
				if (!Number.isSafeInteger(requestedLimit) || requestedLimit < 1 || requestedLimit > 32) throw new Error("max_concurrency must be an integer from 1 through 32");
				const runId = `swarm-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
				const sharedInstructions = typeof args.shared_instructions === "string" && args.shared_instructions.trim() !== "" ? args.shared_instructions.trim() : "Work independently, inspect the current files before editing, keep changes within the declared scope, run focused checks, and report the result to the Lead.";
				const defaults = {
					llm_provider: typeof args.default_llm_provider === "string" ? args.default_llm_provider.trim() : "",
					model: typeof args.default_model === "string" ? args.default_model.trim() : "",
					reasoning_effort: typeof args.default_reasoning_effort === "string" ? args.default_reasoning_effort.trim() : "",
					max_tokens: args.default_max_tokens
				};
				const outcomes = await runWithConcurrency(items, requestedLimit, async (item, index) => {
					const context = item.context ?? args.default_context ?? "fresh";
					const description = typeof item.description === "string" && item.description.trim() !== "" ? item.description.trim() : (typeof item.role === "string" && item.role.trim() !== "" ? item.role.trim() : "Swarm specialist");
					return await ctx.agentTeams.spawnTeammate(caller, {
						name: item.name,
						description: `[swarm:${runId}] ${description}`,
						prompt: [{ type: "text", text: swarmPrompt({ runId, index, total: items.length, item, sharedInstructions }) }],
						context,
						provider: context === "fork" ? config.forkProvider : config.freshProvider,
						agentOptions: routeOptions(item, defaults),
						signal: exec.signal
					});
				});
				const accepted = [];
				const rejected = [];
				outcomes.forEach((outcome, index) => {
					const item = items[index];
					if (outcome?.error !== void 0) {
						rejected.push({ name: item.name, task: item.task, error: safeSwarmError(outcome.error) });
						return;
					}
					accepted.push({
						name: item.name,
						task: item.task,
						...(typeof item.llm_provider === "string" && item.llm_provider.trim() !== "" ? { llmProvider: item.llm_provider.trim() } : defaults.llm_provider === "" ? {} : { llmProvider: defaults.llm_provider }),
						...(typeof item.model === "string" && item.model.trim() !== "" ? { model: item.model.trim() } : defaults.model === "" ? {} : { model: defaults.model }),
						member: outcome.member
					});
				});
				swarmRuns.set(runId, { names: accepted.map((entry) => entry.name) });
				return { run_id: runId, max_concurrency: requestedLimit, accepted, rejected };
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "swarm_wait",
			description: "Wait for every admitted worker in one swarm run to settle, then return its current statuses. This does not wake inactive workers.",
			parameters: {
				run_id: { type: "string", required: true, description: "run_id returned by swarm_start." },
				timeout_ms: { type: "integer", description: "Maximum wait in milliseconds; defaults to 120000." }
			},
			output: jsonOutput(SWARM_WAIT_SCHEMA),
			async execute(args, exec) {
				const caller = callingAgent(exec.agent, "swarm_wait");
				const run = swarmRunFor(caller, args.run_id, swarmRuns, ctx);
				if (run === void 0) throw new Error(`unknown Swarm run: ${args.run_id}`);
				const timeoutMs = args.timeout_ms ?? 120000;
				if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 10000 || timeoutMs > 3600000) throw new Error("timeout_ms must be an integer from 10000 through 3600000");
				const deadline = Date.now() + timeoutMs;
				let timedOut = false;
				let members = ctx.agentTeams.listMembers(caller).filter((member) => run.names.includes(member.name));
				while (members.some((member) => member.status === "running" || member.status === "provisioning")) {
					const remaining = deadline - Date.now();
					if (remaining <= 0) { timedOut = true; break; }
					await ctx.agentTeams.waitForChange(caller, Math.min(30000, remaining), exec.signal);
					members = ctx.agentTeams.listMembers(caller).filter((member) => run.names.includes(member.name));
				}
				return { run_id: args.run_id, timed_out: timedOut, members };
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "swarm_resume",
			description: "Send durable follow-up prompts to selected workers from a swarm run. Inactive workers are cold-resumed and keep their original model route.",
			parameters: {
				run_id: { type: "string", description: "Optional run_id used to validate targets." },
				targets: {
					type: "array",
					required: true,
					items: {
						type: "object",
						additionalProperties: false,
						properties: { target: { type: "string", required: true }, message: { type: "string", required: true } }
					}
				}
			},
			output: jsonOutput(SWARM_RESUME_SCHEMA),
			async execute(args, exec) {
				const caller = callingAgent(exec.agent, "swarm_resume");
				if (args.run_id !== void 0 && swarmRunFor(caller, args.run_id, swarmRuns, ctx) === void 0) throw new Error(`unknown Swarm run: ${args.run_id}`);
				const accepted = await Promise.all(args.targets.map(async (entry) => {
					const result = await ctx.agentTeams.sendMessage(caller, { target: entry.target, content: [{ type: "text", text: entry.message }], signal: exec.signal });
					return { target: entry.target, ...result };
				}));
				return { accepted };
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "send_message",
			description: "Send one durable message to another Team member. A running target receives it at the nearest step boundary; an idle target starts a turn; an inactive teammate cold-resumes.",
			parameters: {
				target: {
					type: "string",
					required: true,
					description: "Team member name, or lead."
				},
				message: {
					type: "string",
					required: true,
					description: "Self-contained message for the target."
				}
			},
			output: jsonOutput(SEND_VALUE_SCHEMA),
			execute(args, exec) {
				return ctx.agentTeams.sendMessage(callingAgent(exec.agent, "send_message"), {
					target: args.target,
					content: [{
						type: "text",
						text: args.message
					}],
					signal: exec.signal
				});
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "list_agents",
			description: "List the Lead and every durable teammate with current runtime status.",
			parameters: {},
			output: jsonOutput(MEMBER_LIST_VALUE_SCHEMA),
			async execute(_args, exec) {
				return Promise.resolve(ctx.agentTeams.listMembers(callingAgent(exec.agent, "list_agents")));
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "wait_agent",
			description: "Wait for the next teammate status, mailbox, or shared-task change after this call starts. This never wakes inactive members and returns noProgress immediately when no other member is running or provisioning. Re-list after wakeup or timeout instead of polling.",
			parameters: { timeout_ms: {
				type: "integer",
				description: "Wait duration in milliseconds, from 10000 through 3600000. Defaults to 30000."
			} },
			output: jsonOutput(WAIT_VALUE_SCHEMA),
			async execute(args, exec) {
				const caller = callingAgent(exec.agent, "wait_agent");
				const timeoutMs = args.timeout_ms ?? 3e4;
				if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1e4 || timeoutMs > 36e5) return await ctx.agentTeams.waitForChange(caller, timeoutMs, exec.signal);
				if (!ctx.agentTeams.listMembers(caller).some((member) => member.id !== caller.id && ACTIVE_WAIT_STATUSES.has(member.status))) return {
					timedOut: false,
					noProgress: {
						reason: "no-active-peer",
						message: NO_ACTIVE_PEER_MESSAGE
					}
				};
				return await ctx.agentTeams.waitForChange(caller, timeoutMs, exec.signal);
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "interrupt_agent",
			description: "Interrupt one teammate's current turn while preserving its pending inbox. Team Lead only.",
			parameters: { target: {
				type: "string",
				required: true,
				description: "Teammate name."
			} },
			output: jsonOutput(INTERRUPT_VALUE_SCHEMA),
			async execute(args, exec) {
				return Promise.resolve(ctx.agentTeams.interrupt(callingAgent(exec.agent, "interrupt_agent"), args.target));
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "team_task_create",
			description: "Create one unowned pending task on the shared Team task board.",
			parameters: {
				subject: {
					type: "string",
					required: true,
					description: "Concise task title."
				},
				description: {
					type: "string",
					required: true,
					description: "Complete task details and acceptance criteria."
				},
				blocked_by: {
					type: "array",
					items: { type: "string" },
					description: "Task ids that must complete first."
				},
				write_scopes: {
					type: "array",
					items: { type: "string" },
					description: "Advisory workspace-relative file or directory prefixes this task expects to modify."
				}
			},
			output: jsonOutput(TASK_VIEW_SCHEMA),
			async execute(args, exec) {
				return await ctx.agentTeams.createTask(callingAgent(exec.agent, "team_task_create"), {
					subject: args.subject,
					description: args.description,
					...args.blocked_by === void 0 ? {} : { blockedBy: args.blocked_by.map(TeamTaskId) },
					...args.write_scopes === void 0 ? {} : { writeScopes: args.write_scopes }
				});
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "team_task_list",
			description: "List shared tasks, including readiness, owner, revision, blockers, and write-scope warnings.",
			parameters: {
				status: {
					type: "string",
					enum: [
						"pending",
						"in_progress",
						"completed"
					],
					description: "Optional exact status filter."
				},
				owner: {
					type: "string",
					description: "Optional member-name filter; use unowned for tasks without an owner."
				},
				ready: {
					type: "boolean",
					description: "Optional readiness filter."
				},
				cursor: {
					type: "integer",
					description: "Zero-based result offset. Defaults to 0."
				},
				limit: {
					type: "integer",
					description: "Number of rows, 1 through 100. Defaults to 50."
				}
			},
			output: jsonOutput(TASK_LIST_VALUE_SCHEMA),
			execute(args, exec) {
				const status = args.status;
				const filtered = ctx.agentTeams.listTasks(callingAgent(exec.agent, "team_task_list")).filter((task) => (status === void 0 || task.status === status) && (args.owner === void 0 || (args.owner === "unowned" ? task.ownerName === void 0 : task.ownerName === args.owner)) && (args.ready === void 0 || task.ready === args.ready));
				const cursor = args.cursor ?? 0;
				const limit = args.limit ?? 50;
				if (!Number.isSafeInteger(cursor) || cursor < 0) throw new Error("cursor must be a non-negative safe integer");
				if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) throw new Error("limit must be an integer from 1 through 100");
				return Promise.resolve({
					tasks: filtered.slice(cursor, cursor + limit),
					...cursor + limit < filtered.length ? { nextCursor: cursor + limit } : {}
				});
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "team_task_get",
			description: "Read the complete latest value of one shared task before changing or executing it.",
			parameters: { task_id: {
				type: "string",
				required: true,
				description: "Shared task id."
			} },
			output: jsonOutput(TASK_VIEW_SCHEMA),
			async execute(args, exec) {
				return Promise.resolve(ctx.agentTeams.getTask(callingAgent(exec.agent, "team_task_get"), TeamTaskId(args.task_id)));
			}
		})));
		register(scoped.tools.register(defineTool({
			name: "team_task_update",
			description: "Compare-and-set a shared task action using the latest revision from team_task_get or team_task_list.",
			parameters: {
				task_id: {
					type: "string",
					required: true,
					description: "Shared task id."
				},
				expected_revision: {
					type: "integer",
					required: true,
					description: "Current task revision used as the CAS precondition."
				},
				action: {
					type: "string",
					required: true,
					enum: [
						"claim",
						"release",
						"edit",
						"set_dependencies",
						"complete",
						"reopen",
						"reassign",
						"delete"
					],
					description: "Task transition to apply."
				},
				subject: {
					type: "string",
					description: "Replacement title for edit."
				},
				description: {
					type: "string",
					description: "Replacement details for edit."
				},
				blocked_by: {
					type: "array",
					items: { type: "string" },
					description: "Complete blocker list for set_dependencies."
				},
				write_scopes: {
					type: "array",
					items: { type: "string" },
					description: "Replacement advisory write scopes for edit."
				},
				owner: {
					type: "string",
					description: "Member name for Lead-only reassign; omit to unassign."
				}
			},
			output: jsonOutput(TASK_VIEW_SCHEMA),
			async execute(args, exec) {
				return await ctx.agentTeams.updateTask(callingAgent(exec.agent, "team_task_update"), {
					taskId: TeamTaskId(args.task_id),
					expectedRevision: args.expected_revision,
					action: args.action,
					...args.subject === void 0 ? {} : { subject: args.subject },
					...args.description === void 0 ? {} : { description: args.description },
					...args.blocked_by === void 0 ? {} : { blockedBy: args.blocked_by.map(TeamTaskId) },
					...args.write_scopes === void 0 ? {} : { writeScopes: args.write_scopes },
					...args.owner === void 0 ? {} : { owner: args.owner }
				});
			}
		})));
	} catch (error) {
		for (const dispose of disposers.reverse()) dispose();
		throw error;
	}
	return () => {
		for (const dispose of disposers.reverse()) dispose();
	};
}
/** Install Team tools in every live or subsequently published Team member scope. */
function apply(ctx, config = {}) {
	const resolved = {
		freshProvider: config.freshProvider ?? "spawn",
		forkProvider: config.forkProvider ?? "fork"
	};
	const installed = /* @__PURE__ */ new Map();
	const maybeInstall = (agent) => {
		if (installed.has(agent)) return;
		const membership = ctx.agentTeams.tryMembership(agent);
		if (membership === void 0) return;
		// One-shot/workflow subagents are also direct children of a Team Lead.
		// Their descriptor is written only when their first turn starts, so the
		// roster probe can temporarily misclassify them as a provisional Lead.
		// The session header is available at publication time; only durable Team
		// teammates (already present in the roster) may receive Team tools/policy.
		if (agent.session.header.origin === "subagent" && membership.role !== "teammate") return;
		installed.set(agent, install(agent, ctx, resolved));
	};
	for (const agent of ctx.agents.list()) maybeInstall(agent);
	ctx.on("agent/created", ({ agent }) => {
		maybeInstall(agent);
	});
	ctx.on("agent/disposed", ({ agent }) => {
		installed.get(agent)?.();
		installed.delete(agent);
	});
	ctx.effect(() => () => {
		for (const dispose of installed.values()) dispose();
		installed.clear();
	}, "tool-team.scopedTools()");
}
//#endregion
export { Config, apply, inject, name };
