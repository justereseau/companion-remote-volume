// Protocol v2 building blocks: capabilities, string targets, structural comparison.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
	ACTIONS,
	MAX_PAYLOAD_BYTES,
	MAX_TARGET_LENGTH,
	PROTOCOL_VERSION,
	TargetNotFoundError,
	hasCapability,
	isValidTarget,
	normalizeCapabilities,
	parseMessage,
	sameValue,
} from '../lib/protocol.js'

test('parseMessage accepts getCapabilities; v1 messages parse exactly as before', () => {
	assert.deepEqual(parseMessage('{"action":"getCapabilities"}'), { ok: true, action: 'getCapabilities' })
	// keys that are not part of an action are ignored, not echoed
	assert.deepEqual(parseMessage('{"action":"mute","app":"spotify","extra":1}'), { ok: true, action: 'mute' })
	assert.deepEqual(parseMessage('{"action":"setVolume","value":5,"device":"x"}'), { ok: true, action: 'setVolume', value: 5 })
})

// The real table has no targeted actions yet (they arrive with the device and app features), so the
// target rules are exercised with a table that declares them.
const TARGETED = {
	...ACTIONS,
	setAppVolume: { validate: (v) => Number.isInteger(v) && v >= 0 && v <= 100, error: 'bad value', target: 'app' },
	muteApp: { target: 'app' },
	setOutputDevice: { target: 'device' },
}
const parse = (obj) => parseMessage(JSON.stringify(obj), TARGETED)

test('target: valid', () => {
	assert.deepEqual(parse({ action: 'setAppVolume', app: 'spotify', value: 40 }), { ok: true, action: 'setAppVolume', value: 40, app: 'spotify' })
	assert.deepEqual(parse({ action: 'muteApp', app: 'system-sounds' }), { ok: true, action: 'muteApp', app: 'system-sounds' })
	assert.deepEqual(parse({ action: 'setOutputDevice', device: '{0.0.0.00000000}.{a1b2}' }), {
		ok: true,
		action: 'setOutputDevice',
		device: '{0.0.0.00000000}.{a1b2}',
	})
	// printable non-ASCII, spaces and shell metacharacters are plain data
	for (const app of ['Café Müller', '日本語', 'a b', '$(rm -rf /)', "x'; y", '😀']) {
		assert.equal(parse({ action: 'muteApp', app }).app, app)
	}
	assert.equal(parse({ action: 'muteApp', app: 'a'.repeat(MAX_TARGET_LENGTH) }).ok, true)
})

test('target: an action reads only the key it declares; other keys are ignored', () => {
	assert.equal(parse({ action: 'muteApp', device: 'speakers' }).ok, false, 'device is not the app target')
	assert.equal(parse({ action: 'setOutputDevice', app: 'spotify' }).ok, false, 'app is not the device target')
	assert.deepEqual(parse({ action: 'muteApp', app: 'spotify', device: 'speakers', junk: [1] }), { ok: true, action: 'muteApp', app: 'spotify' })
})

test('target: missing', () => {
	for (const obj of [{ action: 'muteApp' }, { action: 'muteApp', app: null }, { action: 'setOutputDevice' }]) {
		const r = parse(obj)
		assert.equal(r.ok, false, JSON.stringify(obj))
		assert.equal(r.action, obj.action)
	}
	assert.match(parse({ action: 'muteApp' }).error, /^Invalid app for muteApp\. Must be a string of 1-256/)
	assert.match(parse({ action: 'setOutputDevice' }).error, /^Invalid device for setOutputDevice\./)
})

test('target: empty', () => {
	assert.equal(parse({ action: 'muteApp', app: '' }).ok, false)
})

test('target: too long', () => {
	assert.equal(parse({ action: 'muteApp', app: 'a'.repeat(MAX_TARGET_LENGTH + 1) }).ok, false)
	// length counts characters, not UTF-16 units: 256 emoji are fine, 257 are not
	assert.equal(isValidTarget('😀'.repeat(MAX_TARGET_LENGTH)), true)
	assert.equal(isValidTarget('😀'.repeat(MAX_TARGET_LENGTH + 1)), false)
})

test('target: control characters', () => {
	for (const bad of ['a\0b', 'a\nb', 'a\rb', 'a\tb', 'a\u001bb', 'a\u007fb', 'a\u0085b', '\0']) {
		assert.equal(parse({ action: 'muteApp', app: bad }).ok, false, JSON.stringify(bad))
		assert.equal(parse({ action: 'setOutputDevice', device: bad }).ok, false, JSON.stringify(bad))
	}
})

test('target: lone surrogates are not valid text', () => {
	assert.equal(parseMessage('{"action":"muteApp","app":"a\\ud800"}', TARGETED).ok, false)
	assert.equal(parseMessage('{"action":"muteApp","app":"\\ud83d\\ude00"}', TARGETED).ok, true)
})

test('target: wrong type', () => {
	for (const app of [42, true, false, {}, [], ['spotify'], { name: 'spotify' }]) {
		assert.equal(parse({ action: 'muteApp', app }).ok, false, JSON.stringify(app))
	}
})

test('target: the value is still checked on targeted actions', () => {
	assert.deepEqual(parse({ action: 'setAppVolume', app: 'spotify', value: 101 }), { ok: false, action: 'setAppVolume', error: 'bad value' })
	assert.equal(parse({ action: 'setAppVolume', app: 'spotify' }).ok, false)
})

test('target: a maximal message still fits the 1 KB payload limit', () => {
	const worst = JSON.stringify({ action: 'increaseAppVolume', app: '€'.repeat(MAX_TARGET_LENGTH), value: 99 })
	assert.ok(Buffer.byteLength(worst) < MAX_PAYLOAD_BYTES, `${Buffer.byteLength(worst)} bytes`)
})

test('TargetNotFoundError carries the protocol error text', () => {
	assert.equal(new TargetNotFoundError('device').message, 'Device not found')
	assert.equal(new TargetNotFoundError('app').message, 'App not found')
	assert.equal(new TargetNotFoundError('app').kind, 'app')
})

test('normalizeCapabilities: defaults, only a literal true counts, nothing else leaks', () => {
	const none = { protocolVersion: PROTOCOL_VERSION, outputDevices: { list: false, select: false }, apps: { volume: false, mute: false } }
	assert.equal(PROTOCOL_VERSION, 2)
	for (const raw of [undefined, null, {}, 'x', 42, [], { apps: null }, { apps: 'yes' }, { outputDevices: { list: 1, select: 'true' } }]) {
		assert.deepEqual(normalizeCapabilities(raw), none, JSON.stringify(raw))
	}
	assert.deepEqual(normalizeCapabilities({ protocolVersion: 7, outputDevices: { list: true, extra: true }, apps: { mute: true }, other: {} }), {
		protocolVersion: 2,
		outputDevices: { list: true, select: false },
		apps: { volume: false, mute: true },
	})
	// key order is the documented one
	assert.deepEqual(Object.keys(normalizeCapabilities({})), ['protocolVersion', 'outputDevices', 'apps'])
})

test('hasCapability reads dotted flags', () => {
	const caps = normalizeCapabilities({ apps: { volume: true } })
	assert.equal(hasCapability(caps, 'apps.volume'), true)
	assert.equal(hasCapability(caps, 'apps.mute'), false)
	assert.equal(hasCapability(caps, 'nope.nothing'), false)
	assert.equal(hasCapability(undefined, 'apps.volume'), false)
})

test('sameValue: scalars compare strictly, lists and objects by content', () => {
	assert.equal(sameValue(1, 1), true)
	assert.equal(sameValue(1, '1'), false)
	assert.equal(sameValue(null, null), true)
	assert.equal(sameValue(null, undefined), false)
	assert.equal(sameValue('a', 'b'), false)
	assert.equal(sameValue(null, []), false)
	assert.equal(sameValue({}, null), false)
	assert.equal(sameValue([], {}), false)

	const apps = [{ id: 'spotify', volume: 40, muted: false }]
	assert.equal(sameValue(apps, structuredClone(apps)), true)
	assert.equal(sameValue(apps, [{ id: 'spotify', volume: 41, muted: false }]), false)
	assert.equal(sameValue(apps, [{ id: 'spotify', muted: false, volume: 40 }]), true, 'key order is irrelevant')
	assert.equal(sameValue(apps, [{ id: 'spotify', volume: 40 }]), false, 'a missing key is a change')
	assert.equal(sameValue([{ a: 1 }], [{ b: 1 }]), false, 'same key count, different keys')
	assert.equal(sameValue([1, 2], [2, 1]), false, 'order of a list is content')
	assert.equal(sameValue([1], [1, 2]), false)
	assert.equal(sameValue([], []), true)
})
