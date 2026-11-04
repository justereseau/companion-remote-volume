// WebSocket protocol helpers: message validation, client admission, rate limiting.
// Kept free of Electron / OS dependencies so it can be unit-tested with `node --test`.

import { timingSafeEqual, createHash } from 'node:crypto'

/** Hard limit for a single incoming frame. Valid messages are < 100 bytes. */
export const MAX_PAYLOAD_BYTES = 1024
/** Maximum number of simultaneously connected clients. */
export const MAX_CLIENTS = 32

const isIntInRange = (min, max) => (v) => Number.isInteger(v) && v >= min && v <= max

/** action -> validator for `value` (null = action takes no value) */
export const ACTIONS = Object.freeze({
	setVolume: { validate: isIntInRange(0, 100), error: 'Invalid value for setVolume. Must be an integer between 0 and 100.' },
	increaseVolume: { validate: isIntInRange(1, 99), error: 'Invalid value for increaseVolume. Must be an integer between 1 and 99.' },
	decreaseVolume: { validate: isIntInRange(1, 99), error: 'Invalid value for decreaseVolume. Must be an integer between 1 and 99.' },
	getState: null,
	isMuted: null,
	mute: null,
	unmute: null,
	toggleMute: null,
})

/**
 * Parse and validate one client message.
 * @returns {{ ok: true, action: string, value?: number } | { ok: false, action?: string, error: string }}
 */
export function parseMessage(raw) {
	let msg
	try {
		msg = JSON.parse(typeof raw === 'string' ? raw : raw.toString('utf8'))
	} catch {
		return { ok: false, error: 'Failed to process message' }
	}
	if (!msg || typeof msg !== 'object' || Array.isArray(msg)) {
		return { ok: false, error: 'Failed to process message' }
	}
	const { action, value } = msg
	if (typeof action !== 'string' || !Object.hasOwn(ACTIONS, action)) {
		return { ok: false, action: typeof action === 'string' ? action.slice(0, 64) : undefined, error: 'Invalid action' }
	}
	const spec = ACTIONS[action]
	if (spec && !spec.validate(value)) {
		return { ok: false, action, error: spec.error }
	}
	return spec ? { ok: true, action, value } : { ok: true, action }
}

const digest = (s) => createHash('sha256').update(String(s), 'utf8').digest()

/** Constant-time string comparison (hashing first equalises lengths). */
export function safeEqual(a, b) {
	return timingSafeEqual(digest(a), digest(b))
}

/** Extract a token from `?token=` or an `Authorization: Bearer` header. */
export function extractToken(req) {
	const auth = req.headers?.authorization
	if (typeof auth === 'string' && auth.startsWith('Bearer ')) return auth.slice(7).trim()
	try {
		return new URL(req.url ?? '/', 'ws://localhost').searchParams.get('token') ?? ''
	} catch {
		return ''
	}
}

/**
 * Decide whether an incoming HTTP upgrade request may become a WebSocket client.
 * @param {{ headers: object, url?: string }} req
 * @param {{ token: string, allowedOrigins: string[] }} wsConfig
 * @param {number} clientCount currently connected clients
 * @returns {{ ok: true } | { ok: false, code: number, reason: string }}
 */
export function admitClient(req, wsConfig, clientCount = 0) {
	if (clientCount >= MAX_CLIENTS) {
		return { ok: false, code: 503, reason: 'Too many clients' }
	}
	// Browsers always send Origin on WebSocket handshakes; native clients normally don't.
	// Blocking unknown origins stops arbitrary web pages from controlling the volume.
	const origin = req.headers?.origin
	if (origin !== undefined && !(wsConfig.allowedOrigins ?? []).includes(origin)) {
		return { ok: false, code: 403, reason: 'Origin not allowed' }
	}
	if (wsConfig.token) {
		const presented = extractToken(req)
		if (!presented || !safeEqual(presented, wsConfig.token)) {
			return { ok: false, code: 401, reason: 'Invalid or missing token' }
		}
	}
	return { ok: true }
}

/**
 * Simple token-bucket limiter, one per connection.
 * Every accepted command spawns an OS process, so unbounded message rates are a DoS vector.
 */
export class RateLimiter {
	constructor({ capacity = 20, refillPerSecond = 20, now = () => Date.now() } = {}) {
		this.capacity = capacity
		this.refillPerMs = refillPerSecond / 1000
		this.tokens = capacity
		this.now = now
		this.last = now()
	}

	tryRemove() {
		const t = this.now()
		this.tokens = Math.min(this.capacity, this.tokens + (t - this.last) * this.refillPerMs)
		this.last = t
		if (this.tokens < 1) return false
		this.tokens -= 1
		return true
	}
}
