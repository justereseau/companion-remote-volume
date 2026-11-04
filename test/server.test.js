// End-to-end tests of the WebSocket server with a fake volume backend (no Electron, no sound card).
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { WebSocket } from 'ws'
import { createVolumeServer } from '../lib/server.js'
import { RateLimiter, ACTIONS, TargetNotFoundError } from '../lib/protocol.js'

function fakeController() {
	const state = { volume: 40, muted: false, outputDevice: 'Speakers' }
	const calls = []
	const counters = { reads: 0 }
	const record =
		(name, fn) =>
		async (...args) => {
			calls.push([name, ...args])
			return fn(...args)
		}
	return {
		state,
		calls,
		counters,
		getVolume: async () => (counters.reads++, state.volume),
		isMuted: async () => state.muted,
		getAudioDevice: async () => state.outputDevice,
		setVolume: record('setVolume', (v) => (state.volume = v)),
		increaseVolume: record('increaseVolume', (v) => (state.volume = Math.min(100, state.volume + v))),
		decreaseVolume: record('decreaseVolume', (v) => (state.volume = Math.max(0, state.volume - v))),
		mute: record('mute', () => (state.muted = true)),
		unmute: record('unmute', () => (state.muted = false)),
		toggleMute: record('toggleMute', () => (state.muted = !state.muted)),
		capabilities: {},
		getCapabilities: async () => structuredClone(controller.capabilities),
	}
}

let config, controller, server, url

async function startServer(overrides = {}) {
	server = createVolumeServer({ getConfig: () => config, controller, ...overrides })
	const addr = await server.start()
	url = `ws://127.0.0.1:${addr.port}`
	return addr
}

beforeEach(async () => {
	config = {
		websocket: { port: 0, host: '127.0.0.1', token: '', allowedOrigins: [] },
		polling: { enabled: false, interval: 1000 },
	}
	controller = fakeController()
	await startServer()
})

afterEach(() => server.stop())

/** Connect and collect JSON messages. Resolves once open (or with the HTTP status on rejection). */
function connect(path = '', options = {}) {
	return new Promise((resolve) => {
		const ws = new WebSocket(url + path, options)
		const messages = []
		const waiters = []
		ws.on('message', (m) => {
			messages.push(JSON.parse(m.toString()))
			waiters.splice(0).forEach((w) => w())
		})
		/** Resolve with the first message (from index `from`) matching `pred`. */
		const until = (pred, { from = 0, timeout = 2000 } = {}) =>
			new Promise((res, rej) => {
				const timer = setTimeout(
					() => rej(new Error(`timeout waiting for message; got ${JSON.stringify(messages)}`)),
					timeout,
				)
				const check = () => {
					const hit = messages.slice(from).find(pred)
					if (hit !== undefined) {
						clearTimeout(timer)
						res(hit)
					} else waiters.push(check)
				}
				check()
			})
		const next = (count = messages.length + 1, timeout) => until((_m) => messages.length >= count, { timeout }).then(() => messages[count - 1])
		ws.on('open', () => resolve({ ws, messages, next, until, status: 101 }))
		ws.on('unexpected-response', (_req, res) => resolve({ status: res.statusCode }))
		ws.on('error', () => {})
	})
}

const send = (c, obj) => c.ws.send(typeof obj === 'string' ? obj : JSON.stringify(obj))
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const NO_CAPABILITIES = { protocolVersion: 2, outputDevices: { list: false, select: false }, apps: { volume: false, mute: false } }
const isFullState = (m) => 'volume' in m && 'muted' in m && 'outputDevice' in m
const eq = (expected) => (m) => JSON.stringify(m) === JSON.stringify(expected)

test('pushes the current state on connect', async () => {
	const c = await connect()
	assert.deepEqual(await c.next(1), { volume: 40, muted: false, outputDevice: 'Speakers', capabilities: NO_CAPABILITIES })
	c.ws.close()
})

test('executes every documented action', async () => {
	const c = await connect()
	await c.next(1)
	for (const msg of [
		{ action: 'setVolume', value: 70 },
		{ action: 'increaseVolume', value: 5 },
		{ action: 'decreaseVolume', value: 10 },
		{ action: 'mute' },
		{ action: 'unmute' },
		{ action: 'toggleMute' },
	])
		send(c, msg)
	await sleep(100)
	send(c, { action: 'getState' })
	assert.deepEqual(await c.until(isFullState, { from: 1 }), { volume: 65, muted: true, outputDevice: 'Speakers' })
	const before = c.messages.length
	send(c, { action: 'isMuted' })
	assert.deepEqual(await c.next(before + 1), { muted: true })
	assert.deepEqual(
		controller.calls.map((x) => x[0]),
		['setVolume', 'increaseVolume', 'decreaseVolume', 'mute', 'unmute', 'toggleMute'],
	)
	c.ws.close()
})

test('commands push the new state to every client even with polling disabled', async () => {
	assert.equal(config.polling.enabled, false)
	const a = await connect()
	const b = await connect()
	await a.next(1)
	await b.next(1)

	send(a, { action: 'setVolume', value: 70 })
	assert.deepEqual(await a.until(eq({ volume: 70 })), { volume: 70 })
	assert.deepEqual(await b.until(eq({ volume: 70 })), { volume: 70 })

	send(b, { action: 'toggleMute' })
	await a.until(eq({ muted: true }))
	await b.until(eq({ muted: true }))

	send(a, { action: 'decreaseVolume', value: 20 })
	await b.until(eq({ volume: 50 }))
	a.ws.close()
	b.ws.close()
})

test('only changed fields are pushed after a command', async () => {
	const c = await connect()
	await c.next(1)
	send(c, { action: 'setVolume', value: 40 }) // same as current: nothing to push
	send(c, { action: 'mute' })
	await c.until(eq({ muted: true }))
	await sleep(50)
	assert.deepEqual(c.messages.slice(1), [{ muted: true }])
	c.ws.close()
})

test('a burst of commands is coalesced and ends with the final value', async () => {
	const c = await connect()
	await c.next(1)
	const readsBefore = controller.counters.reads
	for (let v = 1; v <= 15; v++) send(c, { action: 'setVolume', value: v })
	await c.until(eq({ volume: 15 }))
	await sleep(50)
	const reads = controller.counters.reads - readsBefore
	assert.ok(reads < 15, `expected coalesced refreshes, got ${reads} state reads for 15 commands`)
	assert.deepEqual(c.messages.at(-1), { volume: 15 })
	c.ws.close()
})

test('failed commands do not push state', async () => {
	controller.mute = async () => {
		throw new Error('helper crashed')
	}
	const c = await connect()
	await c.next(1)
	send(c, { action: 'mute' })
	assert.deepEqual(await c.next(2), { action: 'mute', response: { error: 'Failed to execute action' } })
	send(c, { action: 'isMuted' })
	assert.deepEqual(await c.next(3), { muted: false })
	c.ws.close()
})

test('a new client brings others up to date with external changes', async () => {
	const a = await connect()
	await a.next(1)
	controller.state.volume = 5 // changed outside the app, polling disabled
	const b = await connect()
	assert.deepEqual(await b.next(1), { volume: 5, muted: false, outputDevice: 'Speakers', capabilities: NO_CAPABILITIES })
	assert.deepEqual(await a.until(eq({ volume: 5 })), { volume: 5 })
	assert.equal(b.messages.length, 1, 'newcomer gets the full state once, not the diff too')
	a.ws.close()
	b.ws.close()
})

test('invalid messages get an error reply and never reach the backend', async () => {
	const c = await connect()
	await c.next(1)
	send(c, { action: 'setVolume', value: 101 })
	send(c, { action: 'setVolume', value: '50' })
	send(c, { action: 'increaseVolume', value: 0 })
	send(c, { action: 'constructor' })
	send(c, 'not json')
	const replies = [await c.next(2), await c.next(3), await c.next(4), await c.next(5), await c.next(6)]
	assert.match(replies[0].response.error, /between 0 and 100/)
	assert.match(replies[1].response.error, /between 0 and 100/)
	assert.match(replies[2].response.error, /between 1 and 99/)
	assert.deepEqual(replies[3], { action: 'constructor', response: { error: 'Invalid action' } })
	assert.deepEqual(replies[4], { error: 'Failed to process message' })
	assert.deepEqual(controller.calls, [])
	c.ws.close()
})

test('binary frames are ignored', async () => {
	const c = await connect()
	await c.next(1)
	c.ws.send(Buffer.from(JSON.stringify({ action: 'mute' })), { binary: true })
	await sleep(50)
	assert.deepEqual(controller.calls, [])
	assert.equal(c.messages.length, 1)
	c.ws.close()
})

test('a failing state read does not crash the server', async () => {
	controller.getVolume = async () => {
		throw new Error('no device')
	}
	const c = await connect()
	send(c, { action: 'mute' }) // triggers a refresh that fails
	await sleep(50)
	assert.equal(c.messages.length, 0)
	assert.equal(c.ws.readyState, WebSocket.OPEN)
	c.ws.close()
})

test('messages over 1 KB close the connection with 1009', async () => {
	const c = await connect()
	await c.next(1)
	const closed = new Promise((r) => c.ws.on('close', (code) => r(code)))
	send(c, { action: 'getState', pad: 'x'.repeat(2000) })
	assert.equal(await closed, 1009)
})

test('browser origins are rejected, allow-listed ones accepted', async () => {
	assert.equal((await connect('', { origin: 'https://evil.example' })).status, 403)
	config.websocket.allowedOrigins = ['http://localhost:8080']
	const ok = await connect('', { origin: 'http://localhost:8080' })
	assert.equal(ok.status, 101)
	ok.ws.close()
})

test('token is enforced when configured', async () => {
	config.websocket.token = 'secret-token'
	assert.equal((await connect()).status, 401)
	assert.equal((await connect('/?token=nope')).status, 401)
	const viaQuery = await connect('/?token=secret-token')
	assert.equal(viaQuery.status, 101)
	const viaHeader = await connect('', { headers: { Authorization: 'Bearer secret-token' } })
	assert.equal(viaHeader.status, 101)
	viaQuery.ws.close()
	viaHeader.ws.close()
})

test('rate limit rejects bursts beyond the bucket', async () => {
	await server.stop()
	await startServer({ createRateLimiter: () => new RateLimiter({ capacity: 5, refillPerSecond: 0.001 }) })
	const c = await connect()
	await c.next(1)
	for (let i = 0; i < 8; i++) send(c, { action: 'isMuted' })
	await c.next(9)
	assert.equal(c.messages.filter((m) => m.error === 'Rate limit exceeded').length, 3)
	c.ws.close()
})

test('refresh broadcasts external changes, one message per changed field', async () => {
	const a = await connect()
	const b = await connect()
	await a.next(1)
	await b.next(1)
	controller.state.volume = 12
	await server.refresh()
	assert.deepEqual(await a.next(2), { volume: 12 })
	assert.deepEqual(await b.next(2), { volume: 12 })
	controller.state.outputDevice = 'Headset'
	controller.state.muted = true
	await server.refresh()
	assert.deepEqual([await a.next(3), await a.next(4)], [{ muted: true }, { outputDevice: 'Headset' }])
	await server.refresh() // nothing changed
	await sleep(50)
	assert.equal(a.messages.length, 4)
	a.ws.close()
	b.ws.close()
})

test('polling timer picks up external changes when enabled', async () => {
	config.polling = { enabled: true, interval: 50 }
	const addr = await server.restart()
	url = `ws://127.0.0.1:${addr.port}`
	const c = await connect()
	await c.next(1)
	controller.state.volume = 99
	assert.deepEqual(await c.until(eq({ volume: 99 })), { volume: 99 })
	c.ws.close()
})

test('without polling, external changes are not pushed on their own', async () => {
	const c = await connect()
	await c.next(1)
	controller.state.volume = 99
	await sleep(200)
	assert.equal(c.messages.length, 1)
	c.ws.close()
})

test('restart applies a new port and disconnects old clients', async () => {
	const c = await connect()
	await c.next(1)
	const closed = new Promise((r) => c.ws.on('close', r))
	const addr = await server.restart()
	await closed
	url = `ws://127.0.0.1:${addr.port}`
	const again = await connect()
	assert.equal(again.status, 101)
	again.ws.close()
})

/** Simulate a settings save: mutate config, then apply it against a snapshot of the old one. */
async function save(mutate) {
	const previous = structuredClone(config)
	mutate(config)
	return server.applyConfig(previous)
}
const closedWith = (c) => new Promise((r) => c.ws.on('close', (code, reason) => r({ code, reason: reason.toString() })))

test('saving unrelated or unchanged settings keeps the server and clients untouched', async () => {
	const c = await connect()
	await c.next(1)
	let closed = false
	c.ws.on('close', () => (closed = true))

	assert.deepEqual(await save(() => {}), { restarted: false, disconnected: 0, pollingRestarted: false })
	assert.deepEqual(await save((cfg) => (cfg.runOnStartup = true)), { restarted: false, disconnected: 0, pollingRestarted: false })
	await sleep(50)
	assert.equal(closed, false)
	send(c, { action: 'isMuted' })
	assert.deepEqual(await c.next(2), { muted: false })
	c.ws.close()
})

test('changing polling only restarts the timer', async () => {
	const c = await connect()
	await c.next(1)
	const result = await save((cfg) => (cfg.polling = { enabled: true, interval: 50 }))
	assert.deepEqual(result, { restarted: false, disconnected: 0, pollingRestarted: true })
	controller.state.volume = 77
	assert.deepEqual(await c.until(eq({ volume: 77 })), { volume: 77 }, 'same connection receives polled changes')
	c.ws.close()
})

test('setting a token disconnects only clients without it', async () => {
	const plain = await connect()
	await plain.next(1)
	const closing = closedWith(plain)
	const result = await save((cfg) => (cfg.websocket.token = 'new-secret'))
	assert.deepEqual(result, { restarted: false, disconnected: 1, pollingRestarted: false })
	assert.deepEqual(await closing, { code: 1008, reason: 'Access settings changed' })
	assert.equal((await connect()).status, 401)
	const authed = await connect('/?token=new-secret')
	await authed.next(1)

	// Changing an unrelated access rule keeps clients that still pass.
	let authedClosed = false
	authed.ws.on('close', () => (authedClosed = true))
	assert.equal((await save((cfg) => (cfg.websocket.allowedOrigins = ['http://localhost:9000']))).disconnected, 0)
	// Removing the token keeps everyone.
	assert.equal((await save((cfg) => (cfg.websocket.token = ''))).disconnected, 0)
	await sleep(50)
	assert.equal(authedClosed, false)
	authed.ws.close()
})

test('removing an allowed origin disconnects that browser client only', async () => {
	config.websocket.allowedOrigins = ['http://localhost:8080']
	const browser = await connect('', { origin: 'http://localhost:8080' })
	const native = await connect()
	await browser.next(1)
	await native.next(1)
	const closing = closedWith(browser)
	assert.equal((await save((cfg) => (cfg.websocket.allowedOrigins = []))).disconnected, 1)
	assert.equal((await closing).code, 1008)
	send(native, { action: 'isMuted' })
	assert.deepEqual(await native.until(eq({ muted: false }), { from: 1 }), { muted: false })
	native.ws.close()
})

test('changing the port or bind address restarts the listener', async () => {
	const c = await connect()
	await c.next(1)
	const closing = new Promise((r) => c.ws.on('close', r))
	const result = await save((cfg) => (cfg.websocket.host = '0.0.0.0'))
	assert.equal(result.restarted, true)
	await closing
})

test('binding to 127.0.0.1 is honoured', async () => {
	await server.stop()
	config.websocket.host = '127.0.0.1'
	const addr = await server.start()
	assert.equal(addr.address, '127.0.0.1')
})

test('start rejects when the port is taken', async () => {
	await server.stop()
	const addr = await server.start()
	const second = createVolumeServer({
		getConfig: () => ({ ...config, websocket: { ...config.websocket, port: addr.port } }),
		controller,
	})
	await assert.rejects(second.start(), /EADDRINUSE/)
	await second.stop()
})

// ---- protocol v2 groundwork ----

test('getState keeps its v1 shape: no capabilities in the reply', async () => {
	const c = await connect()
	await c.next(1)
	send(c, { action: 'getState' })
	assert.deepEqual(await c.next(2), { volume: 40, muted: false, outputDevice: 'Speakers' })
	c.ws.close()
})

test("getCapabilities replies with the controller's runtime answer", async () => {
	const c = await connect()
	await c.next(1)
	send(c, { action: 'getCapabilities' })
	assert.deepEqual(await c.next(2), { capabilities: NO_CAPABILITIES })

	controller.capabilities = { outputDevices: { list: true, select: true }, apps: { volume: true, mute: true } }
	send(c, { action: 'getCapabilities' })
	assert.deepEqual(await c.next(3), {
		capabilities: { protocolVersion: 2, outputDevices: { list: true, select: true }, apps: { volume: true, mute: true } },
	})
	assert.deepEqual(controller.calls, [], 'a capability query does not touch the volume')
	c.ws.close()
})

test('the initial state carries the same capabilities object', async () => {
	controller.capabilities = { apps: { volume: true } }
	const c = await connect()
	const first = await c.next(1)
	assert.deepEqual(first.capabilities, { protocolVersion: 2, outputDevices: { list: false, select: false }, apps: { volume: true, mute: false } })
	send(c, { action: 'getCapabilities' })
	assert.deepEqual((await c.next(2)).capabilities, first.capabilities)
	c.ws.close()
})

test('capabilities are never broadcast as state changes', async () => {
	const c = await connect()
	await c.next(1)
	controller.capabilities = { apps: { volume: true } }
	controller.state.volume = 41
	await server.refresh()
	await sleep(50)
	assert.deepEqual(c.messages.slice(1), [{ volume: 41 }])
	c.ws.close()
})

test('a controller without getCapabilities reports nothing supported', async () => {
	delete controller.getCapabilities
	const c = await connect()
	assert.deepEqual((await c.next(1)).capabilities, NO_CAPABILITIES)
	c.ws.close()
})

test('a failing capability probe does not cost the client its state', async () => {
	controller.getCapabilities = async () => {
		throw new Error('pulse died')
	}
	const c = await connect()
	assert.deepEqual(await c.next(1), { volume: 40, muted: false, outputDevice: 'Speakers' })
	send(c, { action: 'getCapabilities' })
	assert.deepEqual(await c.next(2), { action: 'getCapabilities', response: { error: 'Failed to execute action' } })
	c.ws.close()
})

// The device and app actions come later. These tests plug stand-ins into the server's extension points
// (actions, commands, extraState) to check the shared machinery: capability gating, string targets,
// not-found errors and structured state.
function v2Fixture() {
	const fx = {
		apps: [{ id: 'spotify', name: 'Spotify', volume: 40, muted: false }],
		calls: [],
		actions: {
			...ACTIONS,
			setAppVolume: {
				validate: (v) => Number.isInteger(v) && v >= 0 && v <= 100,
				error: 'bad value',
				target: 'app',
				requires: 'apps.volume',
				mutates: true,
			},
			getApps: { requires: 'apps.volume', reply: true },
		},
		commands: {
			setAppVolume: async (value, msg) => {
				fx.calls.push(['setAppVolume', msg.app, value])
				if (fx.failWith) throw fx.failWith
				const app = fx.apps.find((a) => a.id === msg.app)
				if (!app) throw new TargetNotFoundError('app')
				app.volume = value
			},
			getApps: async () => ({ apps: fx.apps }),
		},
		extraState: { apps: () => fx.apps },
	}
	return fx
}

async function startV2(fx) {
	await server.stop()
	await startServer({ actions: fx.actions, commands: fx.commands, extraState: fx.extraState })
}

test('actions that need an unsupported capability get "Not supported on this OS" and never run', async () => {
	const fx = v2Fixture()
	await startV2(fx)
	const c = await connect()
	await c.next(1)
	send(c, { action: 'setAppVolume', app: 'spotify', value: 10 })
	send(c, { action: 'getApps' })
	assert.deepEqual(await c.next(2), { action: 'setAppVolume', response: { error: 'Not supported on this OS' } })
	assert.deepEqual(await c.next(3), { action: 'getApps', response: { error: 'Not supported on this OS' } })
	assert.deepEqual(fx.calls, [])
	assert.equal(fx.apps[0].volume, 40)
	c.ws.close()
})

test('capability gating follows the runtime answer, and commands push the resulting list', async () => {
	const fx = v2Fixture()
	await startV2(fx)
	const c = await connect()
	await c.next(1)
	controller.capabilities = { apps: { volume: true } } // e.g. the Pulse server came up
	send(c, { action: 'setAppVolume', app: 'spotify', value: 10 })
	await c.until(eq({ apps: [{ id: 'spotify', name: 'Spotify', volume: 10, muted: false }] }))
	assert.deepEqual(fx.calls, [['setAppVolume', 'spotify', 10]])
	send(c, { action: 'getApps' })
	assert.deepEqual((await c.until((m) => 'apps' in m, { from: c.messages.length })).apps[0].volume, 10)
	c.ws.close()
})

test('target validation errors come back before any capability or controller work', async () => {
	const fx = v2Fixture()
	await startV2(fx)
	controller.capabilities = { apps: { volume: true } }
	const c = await connect()
	await c.next(1)
	send(c, { action: 'setAppVolume', value: 10 })
	send(c, { action: 'setAppVolume', app: 'a\u0000b', value: 10 })
	send(c, { action: 'setAppVolume', app: 7, value: 10 })
	for (const n of [2, 3, 4]) {
		const reply = await c.next(n)
		assert.equal(reply.action, 'setAppVolume')
		assert.match(reply.response.error, /^Invalid app for setAppVolume\./)
	}
	assert.deepEqual(fx.calls, [])
	c.ws.close()
})

test('an unknown target gets its own error, not "Failed to execute action"', async () => {
	const fx = v2Fixture()
	await startV2(fx)
	controller.capabilities = { apps: { volume: true } }
	const c = await connect()
	await c.next(1)
	send(c, { action: 'setAppVolume', app: 'nope', value: 10 })
	assert.deepEqual(await c.next(2), { action: 'setAppVolume', response: { error: 'App not found' } })
	fx.failWith = new TargetNotFoundError('device')
	send(c, { action: 'setAppVolume', app: 'spotify', value: 10 })
	assert.deepEqual(await c.next(3), { action: 'setAppVolume', response: { error: 'Device not found' } })
	fx.failWith = new Error('boom')
	send(c, { action: 'setAppVolume', app: 'spotify', value: 10 })
	assert.deepEqual(await c.next(4), { action: 'setAppVolume', response: { error: 'Failed to execute action' } })
	c.ws.close()
})

test('list-valued state is broadcast only when its content changes, one message per field', async () => {
	const fx = v2Fixture()
	await startV2(fx)
	const a = await connect()
	const b = await connect()
	assert.deepEqual((await a.next(1)).apps, fx.apps)
	await b.next(1)

	// a new array with the same content is not a change
	fx.apps = structuredClone(fx.apps)
	await server.refresh()
	await sleep(50)
	assert.equal(a.messages.length, 1)

	// a change inside an element is
	fx.apps = [{ ...fx.apps[0], volume: 55 }]
	await server.refresh()
	const changed = { apps: [{ id: 'spotify', name: 'Spotify', volume: 55, muted: false }] }
	assert.deepEqual(await a.next(2), changed)
	assert.deepEqual(await b.next(2), changed)

	// an app starting together with a scalar change is still one message per field
	fx.apps = [...fx.apps, { id: 'discord', name: 'Discord', volume: 100, muted: true }]
	controller.state.volume = 12
	await server.refresh()
	assert.deepEqual(await a.next(3), { volume: 12 })
	assert.equal((await a.next(4)).apps.length, 2)

	// an app stopping, then nothing changing
	fx.apps = fx.apps.slice(1)
	await server.refresh()
	assert.deepEqual(
		(await a.next(5)).apps.map((x) => x.id),
		['discord'],
	)
	await server.refresh()
	await sleep(50)
	assert.equal(a.messages.length, 5)
	a.ws.close()
	b.ws.close()
})

test('mutating the list in place is still noticed', async () => {
	const fx = v2Fixture()
	await startV2(fx)
	const c = await connect()
	await c.next(1)
	fx.apps[0].volume = 99 // the very array that was handed out before
	await server.refresh()
	assert.equal((await c.next(2)).apps[0].volume, 99)
	c.ws.close()
})

test('lists are part of getState and the initial state; late joiners bring others up to date', async () => {
	const fx = v2Fixture()
	await startV2(fx)
	const a = await connect()
	await a.next(1)
	fx.apps = []
	const b = await connect()
	assert.deepEqual((await b.next(1)).apps, [])
	assert.deepEqual(await a.until((m) => 'apps' in m, { from: 1 }), { apps: [] })
	send(b, { action: 'getState' })
	assert.deepEqual(await b.next(2), { volume: 40, muted: false, outputDevice: 'Speakers', apps: [] })
	a.ws.close()
	b.ws.close()
})

test('restart forgets what clients were told, so lists are re-sent', async () => {
	const fx = v2Fixture()
	await startV2(fx)
	const addr = await server.restart()
	url = `ws://127.0.0.1:${addr.port}`
	const c = await connect()
	assert.deepEqual((await c.next(1)).apps, fx.apps)
	c.ws.close()
})
