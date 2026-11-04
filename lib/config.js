// Configuration defaults and validation.
// Everything coming from the settings window (IPC) or from disk (electron-store)
// is treated as untrusted and normalised here before the main process uses it.

export const CONFIG_VERSION = '1.1.0'

export const BIND_ADDRESSES = ['0.0.0.0', '127.0.0.1']

export const DEFAULT_CONFIG = Object.freeze({
	websocket: Object.freeze({
		port: 2501,
		// '0.0.0.0' keeps the original behaviour (reachable from the LAN, e.g. a Companion
		// machine). '127.0.0.1' restricts control to clients on this computer.
		host: '0.0.0.0',
		// Optional shared secret. Empty string = no token required (original behaviour).
		token: '',
		// Browser origins that may connect. Native clients (Companion, scripts) send no
		// Origin header and are unaffected. Empty list = reject every browser page, which
		// prevents drive-by websites from changing the volume (cross-site WebSocket hijacking).
		allowedOrigins: Object.freeze([]),
	}),
	runOnStartup: false,
	polling: Object.freeze({ enabled: false, interval: 1000 }),
	version: CONFIG_VERSION,
})

const clampInt = (value, min, max, fallback) => {
	const n = typeof value === 'string' && value.trim() !== '' ? Number(value) : value
	if (!Number.isInteger(n)) return fallback
	return Math.min(Math.max(n, min), max)
}

const sanitizeToken = (value) => {
	if (typeof value !== 'string') return ''
	// Printable ASCII only, bounded length – the token travels in a URL query string.
	const trimmed = value.trim()
	return /^[\x21-\x7e]{0,256}$/.test(trimmed) ? trimmed : ''
}

const sanitizeOrigins = (value) => {
	if (!Array.isArray(value)) return []
	return value
		.filter((o) => typeof o === 'string')
		.map((o) => o.trim())
		.filter((o) => /^https?:\/\/[^\s/]+$/i.test(o))
		.slice(0, 32)
}

/**
 * Returns a fresh, fully populated, validated config object.
 * Unknown keys are dropped; invalid values fall back to the previous value or the default.
 */
export function sanitizeConfig(input, previous = DEFAULT_CONFIG) {
	const src = input && typeof input === 'object' ? input : {}
	const prev = previous && typeof previous === 'object' ? previous : DEFAULT_CONFIG
	const ws = src.websocket && typeof src.websocket === 'object' ? src.websocket : {}
	const prevWs = prev.websocket || DEFAULT_CONFIG.websocket
	const polling = src.polling && typeof src.polling === 'object' ? src.polling : {}
	const prevPolling = prev.polling || DEFAULT_CONFIG.polling

	return {
		websocket: {
			port: clampInt(ws.port, 1, 65535, prevWs.port ?? DEFAULT_CONFIG.websocket.port),
			host: BIND_ADDRESSES.includes(ws.host) ? ws.host : (prevWs.host ?? DEFAULT_CONFIG.websocket.host),
			token: 'token' in ws ? sanitizeToken(ws.token) : sanitizeToken(prevWs.token),
			allowedOrigins: 'allowedOrigins' in ws ? sanitizeOrigins(ws.allowedOrigins) : sanitizeOrigins(prevWs.allowedOrigins),
		},
		runOnStartup: typeof src.runOnStartup === 'boolean' ? src.runOnStartup : Boolean(prev.runOnStartup),
		polling: {
			enabled: typeof polling.enabled === 'boolean' ? polling.enabled : Boolean(prevPolling.enabled),
			interval: clampInt(polling.interval, 100, 10000, prevPolling.interval ?? DEFAULT_CONFIG.polling.interval),
		},
		version: CONFIG_VERSION,
	}
}
