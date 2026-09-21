const baseURL = (process.env.PEACHSH_DISCOVERY_BASE_URL ?? 'https://xpeach.codes/v1').replace(/\/$/u, '');
const key = process.env.PEACHSH_DISCOVERY_KEY;
if (!key) throw new Error('Set PEACHSH_DISCOVERY_KEY in the current shell. The value is never printed.');

const response = await fetch(`${baseURL}/models`, { headers: { authorization: `Bearer ${key}` } });
if (!response.ok) throw new Error(`model discovery failed with HTTP ${response.status}; check the API root and use an inference API token`);
const payload = await response.json();
const models = Array.isArray(payload.data) ? payload.data : Array.isArray(payload.models) ? payload.models : [];
const ids = models.map((item) => typeof item === 'string' ? item : item?.id).filter((id) => typeof id === 'string' && id.length > 0);
if (ids.length === 0) throw new Error('model discovery returned no model ids');
console.log(JSON.stringify({ baseURL, models: ids }));
