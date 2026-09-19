/**
 * fetch — a WebFetch-style tool, modelled on Claude Code's.
 *
 * The model calls `fetch` with a URL (and optionally a prompt). The page is
 * downloaded, HTML is converted to a compact Markdown-ish text, and the result
 * is truncated to a sensible size. When a `prompt` is supplied the fetched
 * content is handed to the current model together with the prompt and only
 * the answer is returned, keeping large pages out of the main context.
 *
 * Usage: pirs -e examples/extensions/fetch.ts
 */

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";
import { Type } from "typebox";

const DEFAULT_TIMEOUT_MS = 30_000;
const DEFAULT_MAX_CHARS = 50_000;
const HARD_MAX_CHARS = 200_000;
const USER_AGENT = "pirs-fetch/1.0 (+https://github.com/earendil-works/pi)";

const parameters = Type.Object({
	url: Type.String({ description: "Absolute http(s) URL to fetch" }),
	prompt: Type.Optional(
		Type.String({
			description:
				"What to extract or answer from the page. When given, the page is processed by the model and only the answer is returned.",
		}),
	),
	raw: Type.Optional(Type.Boolean({ description: "Return the raw body without HTML-to-text conversion (default false)" })),
	max_length: Type.Optional(
		Type.Integer({ description: `Maximum characters of content to return (default ${DEFAULT_MAX_CHARS})` }),
	),
	timeout_ms: Type.Optional(Type.Integer({ description: `Request timeout in milliseconds (default ${DEFAULT_TIMEOUT_MS})` })),
});

interface FetchDetails {
	url: string;
	status?: number;
	contentType?: string;
	bytes?: number;
	returnedChars?: number;
	truncated?: boolean;
	processedWithPrompt?: boolean;
	error?: string;
}

// ---------------------------------------------------------------------------
// HTML -> text
// ---------------------------------------------------------------------------

const ENTITIES: Record<string, string> = {
	amp: "&",
	lt: "<",
	gt: ">",
	quot: '"',
	apos: "'",
	nbsp: " ",
	copy: "©",
	reg: "®",
	hellip: "…",
	mdash: "—",
	ndash: "–",
	lsquo: "‘",
	rsquo: "’",
	ldquo: "“",
	rdquo: "”",
};

function decodeEntities(s: string): string {
	return s.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (m, code: string) => {
		if (code[0] === "#") {
			const n = code[1].toLowerCase() === "x" ? parseInt(code.slice(2), 16) : parseInt(code.slice(1), 10);
			return Number.isFinite(n) ? String.fromCodePoint(n) : m;
		}
		return ENTITIES[code.toLowerCase()] ?? m;
	});
}

function htmlToText(html: string): string {
	let s = html;
	// Drop non-content sections entirely.
	s = s.replace(/<!--[\s\S]*?-->/g, "");
	s = s.replace(/<(script|style|noscript|svg|canvas|template|iframe|head)\b[^>]*>[\s\S]*?<\/\1>/gi, "");
	s = s.replace(/<(nav|footer|aside)\b[^>]*>[\s\S]*?<\/\1>/gi, "");

	// Block-level structure -> Markdown-ish.
	s = s.replace(/<h([1-6])\b[^>]*>([\s\S]*?)<\/h\1>/gi, (_m, lvl: string, inner: string) => `\n\n${"#".repeat(Number(lvl))} ${inner.trim()}\n\n`);
	s = s.replace(/<pre\b[^>]*>([\s\S]*?)<\/pre>/gi, (_m, inner: string) => `\n\n\`\`\`\n${inner.replace(/<[^>]+>/g, "")}\n\`\`\`\n\n`);
	s = s.replace(/<code\b[^>]*>([\s\S]*?)<\/code>/gi, (_m, inner: string) => `\`${inner}\``);
	s = s.replace(/<li\b[^>]*>([\s\S]*?)<\/li>/gi, (_m, inner: string) => `\n- ${inner.trim()}`);
	s = s.replace(/<(?:ul|ol)\b[^>]*>/gi, "\n").replace(/<\/(?:ul|ol)>/gi, "\n\n");
	s = s.replace(/<(?:br|hr)\s*\/?>/gi, "\n");
	s = s.replace(/<\/(?:p|div|section|article|main|header|blockquote|tr|table|dd|dt|dl|figure|figcaption)>/gi, "\n\n");
	s = s.replace(/<\/(?:td|th)>/gi, " | ");
	s = s.replace(/<a\b[^>]*href=["']([^"']+)["'][^>]*>([\s\S]*?)<\/a>/gi, (_m, href: string, inner: string) => {
		const text = inner.replace(/<[^>]+>/g, "").trim();
		if (!text || href.startsWith("#") || href.startsWith("javascript:")) return text;
		return text === href ? href : `[${text}](${href})`;
	});
	s = s.replace(/<img\b[^>]*alt=["']([^"']*)["'][^>]*>/gi, (_m, alt: string) => (alt ? `![${alt}]` : ""));
	s = s.replace(/<(?:strong|b)\b[^>]*>([\s\S]*?)<\/(?:strong|b)>/gi, "**$1**");
	s = s.replace(/<(?:em|i)\b[^>]*>([\s\S]*?)<\/(?:em|i)>/gi, "_$1_");

	// Remaining tags.
	s = s.replace(/<[^>]+>/g, "");
	s = decodeEntities(s);

	// Whitespace cleanup.
	s = s.replace(/\r\n?/g, "\n");
	s = s
		.split("\n")
		.map((line) => line.replace(/[ \t\f\v]+/g, " ").trim())
		.join("\n");
	s = s.replace(/\n{3,}/g, "\n\n");
	return s.trim();
}

function extractTitle(html: string): string | undefined {
	const m = html.match(/<title\b[^>]*>([\s\S]*?)<\/title>/i);
	return m ? decodeEntities(m[1].replace(/\s+/g, " ").trim()) : undefined;
}

// ---------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------

// The extension runtime has no global `URL`, so validate by hand.
function validateUrl(raw: string): string {
	const url = raw.trim();
	const scheme = url.match(/^([a-z][a-z0-9+.-]*):/i)?.[1]?.toLowerCase();
	if (!scheme) throw new Error(`Invalid URL: ${raw} (must be absolute, e.g. https://example.com/)`);
	if (scheme !== "http" && scheme !== "https") {
		throw new Error(`Unsupported URL scheme "${scheme}:" — only http and https are allowed`);
	}
	if (!/^https?:\/\/[^\s/?#]+/i.test(url)) throw new Error(`Invalid URL: ${raw}`);
	return url;
}

async function fetchPage(
	url: string,
	timeoutMs: number,
	outerSignal: AbortSignal | undefined,
): Promise<{ status: number; contentType: string; body: string }> {
	const controller = new AbortController();
	const timer = setTimeout(() => controller.abort(), timeoutMs);
	const onAbort = () => controller.abort();
	outerSignal?.addEventListener("abort", onAbort, { once: true });
	try {
		const res = await fetch(url, {
			method: "GET",
			headers: {
				"User-Agent": USER_AGENT,
				Accept: "text/html,application/xhtml+xml,application/json,text/plain,text/*;q=0.9,*/*;q=0.5",
				"Accept-Language": "en",
			},
			signal: controller.signal,
		});
		const body = await res.text();
		const contentType = (res.headers.get("content-type") || "").toLowerCase();
		return { status: res.status, contentType, body };
	} catch (err: any) {
		if (outerSignal?.aborted) throw new Error("Fetch aborted");
		if (controller.signal.aborted) throw new Error(`Fetch timed out after ${timeoutMs}ms`);
		throw new Error(`Fetch failed: ${err?.message ?? String(err)}`);
	} finally {
		clearTimeout(timer);
		outerSignal?.removeEventListener("abort", onAbort);
	}
}

function toContent(body: string, contentType: string, raw: boolean): string {
	if (raw) return body;
	if (contentType.includes("html") || (!contentType && /<html|<body|<div|<p\b/i.test(body))) {
		const title = extractTitle(body);
		const text = htmlToText(body);
		// Skip the <title> when the page already opens with an equivalent heading.
		const firstHeading = text.match(/^#+\s+(.+)$/m)?.[1]?.trim();
		return title && title !== firstHeading ? `# ${title}\n\n${text}` : text;
	}
	if (contentType.includes("json")) {
		try {
			return JSON.stringify(JSON.parse(body), null, 2);
		} catch {
			return body;
		}
	}
	return body;
}

function truncate(text: string, max: number): { text: string; truncated: boolean } {
	if (text.length <= max) return { text, truncated: false };
	return { text: `${text.slice(0, max)}\n\n[... truncated ${text.length - max} characters; total ${text.length}]`, truncated: true };
}

async function processWithModel(ctx: ExtensionContext, url: string, content: string, prompt: string): Promise<string> {
	if (!ctx.model) throw new Error("No model selected; cannot process the page with a prompt");
	const systemPrompt =
		"You are given the contents of a web page and a question or instruction about it. " +
		"Answer using only the page contents. Be concise and preserve code, URLs, and exact values verbatim when relevant. " +
		"If the page does not contain the information, say so.";
	const userText = `URL: ${url}\n\n<page>\n${content}\n</page>\n\nInstruction: ${prompt}`;
	const res: any = await ctx.modelRegistry.complete(
		ctx.model,
		{ systemPrompt, messages: [{ role: "user", content: [{ type: "text", text: userText }], timestamp: Date.now() }] },
		{ maxTokens: 4096 },
	);
	if (res?.stopReason === "error") throw new Error(`Model processing failed: ${res.errorMessage ?? "unknown error"}`);
	const text = (res?.content ?? [])
		.filter((c: any) => c.type === "text")
		.map((c: any) => c.text)
		.join("\n")
		.trim();
	return text || "(model returned no text)";
}

// ---------------------------------------------------------------------------
// Extension
// ---------------------------------------------------------------------------

export default function (pi: ExtensionAPI) {
	pi.registerTool({
		name: "fetch",
		label: "Fetch",
		description:
			"Fetch a web page or API endpoint over HTTP(S) and return its content as text. HTML is converted to Markdown-style text; JSON is pretty-printed. " +
			"Provide `prompt` to have the page processed and only the answer returned, which is preferable for large pages.",
		promptSnippet: "Fetch a URL and return its content (optionally answering a prompt about it)",
		promptGuidelines: [
			"Use fetch to read documentation, READMEs, APIs, or any web page the user references by URL.",
			"When only part of a large page matters, pass a `prompt` describing what you need instead of reading the whole page.",
		],
		parameters,

		async execute(_toolCallId, params, signal, onUpdate, ctx) {
			const url = validateUrl(params.url);
			const timeoutMs = params.timeout_ms ?? DEFAULT_TIMEOUT_MS;
			const maxChars = Math.min(params.max_length ?? DEFAULT_MAX_CHARS, HARD_MAX_CHARS);
			const details: FetchDetails = { url };

			onUpdate?.({ content: [{ type: "text", text: `Fetching ${url}` }], details });

			const page = await fetchPage(url, timeoutMs, signal);
			details.status = page.status;
			details.contentType = page.contentType || undefined;
			details.bytes = page.body.length;

			if (page.status >= 400) {
				const snippet = truncate(toContent(page.body, page.contentType, false), 2_000).text;
				throw new Error(`HTTP ${page.status} fetching ${url}${snippet ? `\n\n${snippet}` : ""}`);
			}

			let content = toContent(page.body, page.contentType, params.raw ?? false);

			if (params.prompt) {
				onUpdate?.({ content: [{ type: "text", text: `Processing ${url} with prompt…` }], details });
				const input = truncate(content, HARD_MAX_CHARS).text;
				content = await processWithModel(ctx, url, input, params.prompt);
				details.processedWithPrompt = true;
			}

			const out = truncate(content, maxChars);
			details.truncated = out.truncated;
			details.returnedChars = out.text.length;

			return { content: [{ type: "text", text: out.text }], details };
		},

		renderCall(args, _theme) {
			const suffix = args.prompt ? ` — "${String(args.prompt).slice(0, 60)}${String(args.prompt).length > 60 ? "…" : ""}"` : "";
			return new Text(`fetch ${args.url}${suffix}`);
		},

		renderResult(result, { expanded }, _theme) {
			const d = (result.details ?? {}) as FetchDetails;
			const head = [d.status !== undefined ? `HTTP ${d.status}` : null, d.contentType, d.bytes !== undefined ? `${d.bytes} bytes` : null]
				.filter(Boolean)
				.join(" · ");
			const flags = [d.processedWithPrompt ? "prompt" : null, d.truncated ? "truncated" : null].filter(Boolean).join(", ");
			const summary = `${head}${flags ? ` (${flags})` : ""}`;
			if (!expanded) return new Text(summary);
			const text = result.content?.find((c: any) => c.type === "text")?.text ?? "";
			return new Text(`${summary}\n${text}`);
		},
	});
}
