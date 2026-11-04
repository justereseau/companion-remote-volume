// End-to-end tests of the WebSocket server with a fake volume backend (no Electron, no sound card).
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { WebSocket } from 'ws'
import { createVolumeServer } from '../lib/server.js'
import { RateLimiter } from '../lib/protocol.js'

function fakeController() {
	const state = { volume: 40, muted: false, outputDevice: 'Speakers' }
	const calls = []
	const record =
		(name, fn) =>
		async (...args) => {
			calls.push([name, ...args])
			return fn(...args)
		}
	return {
		state,
		calls,
		fail: null,
		getVolume: async () => state.volume,
		isMuted: async () => state.muted,
		getAudioDevice: async () => state.outputDevice,
		setVolume: record('setVolume', (v) => (state.volume = v)),
		increaseVolume: record('increaseVolume', (v) => (state.volume = Math.min(100, state.volume + v))),
		decreaseVolume: record('decreaseVolume', (v) => (state.volume = Math.max(0, state.volume - v))),
		mute: record('mute', () => (state.muted = true)),
		unmute: record('unmute', () => (state.muted = false)),
		toggleMute: record('toggleMute', () => (state.muted = !state.muted)),
	}
}

let config, controller, server, url

beforeEach(async () => {
	config = {
		websocket: { port: 0, host: '127.0.0.1', token: '', allowedOrigins: [] },
		polling: { enabled: false, interval: 1000 },
	}
	controller = fakeController()
	server = createVolumeServer({ getConfig: () => config, controller })
	const addr = await server.start()
	url = `ws://127.0.0.1:${addr.port}`
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
		const next = (count = messages.length + 1, timeout = 2000) =>
			new Promise((res, rej) => {
				const check = () => (messages.length >= count ? res(messages[count - 1]) : waiters.push(check))
				setTimeout(() => rej(new Error(`timeout waiting for message #${count}; got ${JSON.stringify(messages)}`)), timeout).unref()
				check()
			})
		ws.on('open', () => resolve({ ws, messages, next, status: 101 }))
		ws.on('unexpected-response', (_req, res) => resolve({ status: res.statusCode }))
		ws.on('error', () => {})
	})
}

const send = (c, obj) => c.ws.send(typeof obj === 'string' ? obj : JSON.stringify(obj))
const settle = () => new Promise((r) => setTimeout(r, 50))

test('pushes the current state on connect', async () => {
	const c = await connect()
	assert.deepEqual(await c.next(1), { volume: 40, muted: false, outputDevice: 'Speakers' })
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
	send(c, { action: 'getState' })
	assert.deepEqual(await c.next(2), { volume: 65, muted: true, outputDevice: 'Speakers' })
	send(c, { action: 'isMuted' })
	assert.deepEqual(await c.next(3), { muted: true })
	assert.deepEqual(
		controller.calls.map((x) => x[0]),
		['setVolume', 'increaseVolume', 'decreaseVolume', 'mute', 'unmute', 'toggleMute'],
	)
	c.ws.close()
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
	await settle()
	assert.deepEqual(controller.calls, [])
	assert.equal(c.messages.length, 1)
	c.ws.close()
})

test('backend errors are reported without killing the connection', async () => {
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

test('a failing initial state read does not crash the server', async () => {
	controller.getVolume = async () => {
		throw new Error('no device')
	}
	const c = await connect()
	await settle()
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
	server = createVolumeServer({
		getConfig: () => config,
		controller,
		createRateLimiter: () => new RateLimiter({ capacity: 5, refillPerSecond: 0.001 }),
	})
	url = `ws://127.0.0.1:${(await server.start()).port}`
	const c = await connect()
	await c.next(1)
	for (let i = 0; i < 8; i++) send(c, { action: 'isMuted' })
	await c.next(9)
	const errors = c.messages.filter((m) => m.error === 'Rate limit exceeded')
	assert.equal(errors.length, 3)
	c.ws.close()
})

test('polling broadcasts only what changed', async () => {
	const a = await connect()
	const b = await connect()
	await a.next(1)
	await b.next(1)
	await server.pollOnce() // establishes the baseline: volume, muted, outputDevice
	await a.next(4)
	controller.state.volume = 12
	await server.pollOnce()
	assert.deepEqual(await a.next(5), { volume: 12 })
	assert.deepEqual(await b.next(5), { volume: 12 })
	controller.state.outputDevice = 'Headset'
	controller.state.muted = true
	await server.pollOnce()
	assert.deepEqual([await a.next(6), await a.next(7)], [{ muted: true }, { outputDevice: 'Headset' }])
	await server.pollOnce() // nothing changed
	await settle()
	assert.equal(a.messages.length, 7)
	a.ws.close()
	b.ws.close()
})

test('polling timer runs when enabled', async () => {
	config.polling = { enabled: true, interval: 50 }
	const addr = await server.restart()
	url = `ws://127.0.0.1:${addr.port}`
	const c = await connect()
	await c.next(1)
	await new Promise((r) => setTimeout(r, 200)) // let the baseline broadcast happen
	controller.state.volume = 99
	await new Promise((r) => setTimeout(r, 300))
	assert.ok(
		c.messages.some((m) => m.volume === 99 && Object.keys(m).length === 1),
		JSON.stringify(c.messages),
	)
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

test('binding to 127.0.0.1 is honoured', async () => {
	await server.stop()
	config.websocket.host = '127.0.0.1'
	const addr = await server.start()
	assert.equal(addr.address, '127.0.0.1')
})

test('start rejects when the port is taken', async () => {
	const addr = await (async () => {
		await server.stop()
		return server.start()
	})()
	const second = createVolumeServer({
		getConfig: () => ({ ...config, websocket: { ...config.websocket, port: addr.port } }),
		controller,
	})
	await assert.rejects(second.start(), /EADDRINUSE/)
	await second.stop()
})
