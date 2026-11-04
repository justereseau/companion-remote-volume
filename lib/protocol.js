// WebSocket protocol helpers: message validation, client admission, rate limiting.
// Kept free of Electron / OS dependencies so it can be unit-tested with `node --test`.

import { timingSafeEqual, createHash } from 'node:crypto'

/** Hard limit for a single incoming frame. Valid messages are < 100 bytes. */
export const MAX_PAYLOAD_BYTES = 1024
/** Maximum number of simultaneously connected clients. */
export const MAX_CLIENTS = 32

/** Protocol version announced in `capabilities.protocolVersion`. v2 only adds to v1. */
export const PROTOCOL_VERSION = 2

/** Error texts that are part of the protocol (clients may match on them). */
export const ERRORS = Object.freeze({
	NOT_SUPPORTED: 'Not supported on this OS',
	DEVICE_NOT_FOUND: 'Device not found',
	APP_NOT_FOUND: 'App not found',
	EXECUTION_FAILED: 'Failed to execute action',
})

/**
 * Thrown by a controller when a command names a device or app that does not exist (any more).
 * The server turns it into `Device not found` / `App not found` instead of `Failed to execute action`.
 */
export class TargetNotFoundError extends Error {
	/** @param {'device' | 'app'} kind */
	constructor(kind) {
		super(kind === 'device' ? ERRORS.DEVICE_NOT_FOUND : ERRORS.APP_NOT_FOUND)
		this.name = 'TargetNotFoundError'
		this.kind = kind
	}
}

/** The capability groups and flags a v2 server reports. Anything not listed here is never sent. */
const CAPABILITY_FLAGS = Object.freeze({
	outputDevices: Object.freeze(['list', 'select']),
	apps: Object.freeze(['volume', 'mute']),
})

/**
 * Build the wire form of `capabilities` from whatever a controller reported. Only a literal `true`
 * counts, so a missing or malformed answer degrades to "not supported" and never to a crash.
 */
export function normalizeCapabilities(raw) {
	const out = { protocolVersion: PROTOCOL_VERSION }
	for (const [group, flags] of Object.entries(CAPABILITY_FLAGS)) {
		out[group] = Object.fromEntries(flags.map((flag) => [flag, raw?.[group]?.[flag] === true]))
	}
	return out
}

/** Whether a normalized capabilities object has the dotted flag, e.g. `hasCapability(caps, 'apps.volume')`. */
export function hasCapability(capabilities, path) {
	const [group, flag] = path.split('.')
	return capabilities?.[group]?.[flag] === true
}

const isIntInRange = (min, max) => (v) => Number.isInteger(v) && v >= min && v <= max

/** Targets are names or ids passed on to native code: bounded, printable, well-formed Unicode. */
export const MAX_TARGET_LENGTH = 256
const CONTROL_CHARS = /\p{Cc}/u
export function isValidTarget(v) {
	if (typeof v !== 'string' || v.length === 0 || !v.isWellFormed() || CONTROL_CHARS.test(v)) return false
	return [...v].length <= MAX_TARGET_LENGTH
}

/** Which message key carries each kind of target, and the error shown when it is absent or invalid. */
export const TARGET_KEYS = Object.freeze({ app: 'app', device: 'device' })
const targetError = (action, key) => `Invalid ${key} for ${action}. Must be a string of 1-${MAX_TARGET_LENGTH} characters without control characters.`

/**
 * action -> spec. `null` = the action takes nothing at all. Otherwise:
 * - `validate` / `error`: check for `value`
 * - `target`: `'app'` or `'device'`, the string key the action needs (unknown keys are ignored)
 * - `requires`: dotted capability, e.g. `'apps.volume'`; the server replies "Not supported" when false
 */
export const ACTIONS = Object.freeze({
	setVolume: { validate: isIntInRange(0, 100), error: 'Invalid value for setVolume. Must be an integer between 0 and 100.' },
	increaseVolume: { validate: isIntInRange(1, 99), error: 'Invalid value for increaseVolume. Must be an integer between 1 and 99.' },
	decreaseVolume: { validate: isIntInRange(1, 99), error: 'Invalid value for decreaseVolume. Must be an integer between 1 and 99.' },
	getState: null,
	getCapabilities: null,
	isMuted: null,
	mute: null,
	unmute: null,
	toggleMute: null,
})

/**
 * Parse and validate one client message.
 * @param {string | Buffer} raw
 * @param {Record<string, object | null>} [actions] action table; only tests pass another one
 * @returns {{ ok: true, action: string, value?: number, app?: string, device?: string } | { ok: false, action?: string, error: string }}
 */
export function parseMessage(raw, actions = ACTIONS) {
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
	if (typeof action !== 'string' || !Object.hasOwn(actions, action)) {
		return { ok: false, action: typeof action === 'string' ? action.slice(0, 64) : undefined, error: 'Invalid action' }
	}
	const spec = actions[action]
	if (!spec) return { ok: true, action }
	if (spec.validate && !spec.validate(value)) {
		return { ok: false, action, error: spec.error }
	}
	const parsed = { ok: true, action }
	if (spec.validate) parsed.value = value
	if (spec.target) {
		const key = TARGET_KEYS[spec.target]
		if (!isValidTarget(msg[key])) return { ok: false, action, error: targetError(action, key) }
		parsed[key] = msg[key]
	}
	return parsed
}

/**
 * Structural equality for state fields: scalars compare with `===` (exactly as before), arrays by
 * position and plain objects by key, recursively. Used to decide whether a list needs re-sending.
 */
export function sameValue(a, b) {
	if (a === b) return true
	if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false
	if (Array.isArray(a) !== Array.isArray(b)) return false
	if (Array.isArray(a)) return a.length === b.length && a.every((v, i) => sameValue(v, b[i]))
	const keys = Object.keys(a)
	return keys.length === Object.keys(b).length && keys.every((k) => Object.hasOwn(b, k) && sameValue(a[k], b[k]))
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
