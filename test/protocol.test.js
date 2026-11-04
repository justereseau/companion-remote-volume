import { test } from 'node:test'
import assert from 'node:assert/strict'
import { parseMessage, admitClient, RateLimiter, extractToken, MAX_CLIENTS } from '../lib/protocol.js'

test('parseMessage accepts valid actions', () => {
	assert.deepEqual(parseMessage('{"action":"setVolume","value":42}'), { ok: true, action: 'setVolume', value: 42 })
	assert.deepEqual(parseMessage(Buffer.from('{"action":"mute"}')), { ok: true, action: 'mute' })
	assert.deepEqual(parseMessage('{"action":"isMuted"}'), { ok: true, action: 'isMuted' })
})

test('parseMessage rejects bad values', () => {
	for (const value of [-1, 101, 50.5, '50', null, undefined, 1e309]) {
		assert.equal(parseMessage(JSON.stringify({ action: 'setVolume', value })).ok, false, `value ${value}`)
	}
	assert.equal(parseMessage('{"action":"increaseVolume","value":0}').ok, false)
	assert.equal(parseMessage('{"action":"decreaseVolume","value":100}').ok, false)
})

test('parseMessage rejects unknown / prototype actions and garbage', () => {
	for (const raw of ['{"action":"constructor"}', '{"action":"__proto__"}', '{"action":"rm"}', 'null', '[]', '"x"', '{', '']) {
		assert.equal(parseMessage(raw).ok, false, raw)
	}
})

const cfg = (over = {}) => ({ token: '', allowedOrigins: [], ...over })

test('admitClient blocks browser origins by default but allows native clients', () => {
	assert.equal(admitClient({ headers: {} }, cfg()).ok, true)
	const r = admitClient({ headers: { origin: 'https://evil.example' } }, cfg())
	assert.equal(r.ok, false)
	assert.equal(r.code, 403)
	assert.equal(admitClient({ headers: { origin: 'http://localhost:8000' } }, cfg({ allowedOrigins: ['http://localhost:8000'] })).ok, true)
})

test('admitClient enforces token when configured', () => {
	const c = cfg({ token: 's3cret' })
	assert.equal(admitClient({ headers: {}, url: '/' }, c).code, 401)
	assert.equal(admitClient({ headers: {}, url: '/?token=wrong' }, c).code, 401)
	assert.equal(admitClient({ headers: {}, url: '/?token=s3cret' }, c).ok, true)
	assert.equal(admitClient({ headers: { authorization: 'Bearer s3cret' }, url: '/' }, c).ok, true)
})

test('admitClient limits number of clients', () => {
	assert.equal(admitClient({ headers: {} }, cfg(), MAX_CLIENTS).code, 503)
})

test('extractToken tolerates malformed urls', () => {
	assert.equal(extractToken({ headers: {}, url: '//[' }), '')
})

test('RateLimiter refills over time', () => {
	let t = 0
	const rl = new RateLimiter({ capacity: 2, refillPerSecond: 1, now: () => t })
	assert.equal(rl.tryRemove(), true)
	assert.equal(rl.tryRemove(), true)
	assert.equal(rl.tryRemove(), false)
	t += 1000
	assert.equal(rl.tryRemove(), true)
})
