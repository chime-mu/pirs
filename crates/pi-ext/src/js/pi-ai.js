export { Type, StringEnum, Value, Kind } from "typebox";
// Streaming helpers route through the host's model registry.
export async function complete(model, context, options) { return globalThis.__pirs.host.async("models.complete", model, context, options || {}); }
export const completeSimple = complete;
export function getModel(provider, id) { return globalThis.__pirs.host.sync("models.get", provider, id); }
export function getModels(provider) { return globalThis.__pirs.host.sync("models.all").filter((m) => !provider || m.provider === provider); }
export function getProviders() { return [...new Set(globalThis.__pirs.host.sync("models.all").map((m) => m.provider))]; }
export function calculateCost(model, usage) { return usage?.cost?.total ?? 0; }
export function supportsXhigh(model) { return !!model?.reasoning; }
export function estimateTokens(text) { return Math.ceil((typeof text === "string" ? text : JSON.stringify(text ?? "")).length / 4); }

export function uuidv7() { return globalThis.__pirs.host.sync("crypto.uuidv7"); }
export function anthropicMessagesApi() { throw new Error("pi-ai provider API implementations are not available inside pirs extensions; register providers with pi.registerProvider(name, { api, baseUrl, apiKey, models })"); }
export const openaiCompletionsApi = anthropicMessagesApi;
export const openAIResponsesApi = anthropicMessagesApi;
export const openAICompletionsApi = anthropicMessagesApi;
export const openAIResponsesStream = anthropicMessagesApi;
export const openaiResponsesApi = anthropicMessagesApi;
export function getEnvApiKey(provider) { return globalThis.__pirs.host.sync("models.providerAuth", provider)?.apiKey; }
export function stream(model, context, options) { return globalThis.__pirs.host.sync("models.stream", model, context, options || {}); }
export const streamSimple = stream;
export function normalizeContext(c) { return c; }
export function getSystemMessageText(m) { return typeof m?.content === "string" ? m.content : (m?.content || []).map((c) => c.text || "").join(""); }
export function createAssistantMessageEventStream() { throw new Error("createAssistantMessageEventStream is not available inside pirs extensions"); }
export function parseStreamingJson(s) { try { return JSON.parse(s); } catch { return {}; } }
export class EventStream { constructor() { throw new Error("EventStream is not available inside pirs extensions"); } }
