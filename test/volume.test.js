import { test } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { backendModuleFor, BACKEND_MODULES, defaultOutputName, createVolumeController } from '../lib/volume.js'

test('every supported OS maps to an existing backend module', async () => {
	for (const osType of ['Windows_NT', 'Darwin', 'Linux']) {
		const url = new URL(backendModuleFor(osType), new URL('../lib/', import.meta.url))
		assert.ok(existsSync(fileURLToPath(url)), `${osType} -> ${url}`)
		const mod = await import(url.href)
		for (const fn of ['getVolume', 'setVolume', 'getMuted', 'setMuted']) {
			assert.equal(typeof mod[fn], 'function', `${osType} backend exports ${fn}`)
		}
	}
	assert.deepEqual(Object.keys(BACKEND_MODULES).sort(), ['Darwin', 'Linux', 'Windows_NT'])
})

test('unsupported OS is rejected', () => {
	assert.throws(() => backendModuleFor('SunOS'), /not supported/)
	assert.throws(() => backendModuleFor('__proto__'), /not supported/)
})

const HOST_APIS = { defaultHostAPI: 0, HostAPIs: [{ name: 'MME', defaultOutput: 3 }] }
const DEVICES = [
	{ id: 0, name: 'Microphone' },
	{ id: 3, name: 'Speakers (HyperX Cloud II)' },
]

test('defaultOutputName finds the default output device by id', () => {
	assert.equal(defaultOutputName(HOST_APIS, DEVICES), 'Speakers (HyperX Cloud II)')
	// Device ids are not array indices.
	assert.equal(defaultOutputName(HOST_APIS, [{ id: 3, name: 'Only one' }]), 'Only one')
})

test('defaultOutputName returns null when there is no output device', () => {
	assert.equal(defaultOutputName({ defaultHostAPI: 0, HostAPIs: [{ defaultOutput: -1 }] }, DEVICES), null)
	assert.equal(defaultOutputName({ defaultHostAPI: 2, HostAPIs: [] }, DEVICES), null)
	assert.equal(defaultOutputName(HOST_APIS, []), null)
	assert.equal(defaultOutputName(undefined, undefined), null)
})

function fakeBackend(initial = { volume: 50, muted: false }) {
	const state = { ...initial }
	const calls = []
	return {
		state,
		calls,
		getVolume: async () => state.volume,
		getMuted: async () => state.muted,
		setVolume: async (v) => {
			calls.push(['setVolume', v])
			state.volume = v
		},
		setMuted: async (m) => {
			calls.push(['setMuted', m])
			state.muted = m
		},
	}
}

const audio = (counter = { n: 0 }) => ({
	getHostAPIs: () => (counter.n++, HOST_APIS),
	getDevices: () => DEVICES,
})

test('increase/decrease clamp to 0..100', async () => {
	const backend = fakeBackend({ volume: 95, muted: false })
	const c = createVolumeController({ backend, audio: audio() })
	await c.increaseVolume(10)
	assert.equal(backend.state.volume, 100)
	await c.decreaseVolume(99)
	assert.equal(backend.state.volume, 1)
	await c.decreaseVolume(5)
	assert.equal(backend.state.volume, 0)
})

test('decreasing to exactly 0 reaches the backend as 0', async () => {
	for (const [start, step] of [
		[2, 2],
		[1, 1],
		[5, 10],
	]) {
		const backend = fakeBackend({ volume: start, muted: false })
		const c = createVolumeController({ backend, audio: audio() })
		await c.decreaseVolume(step)
		assert.deepEqual(backend.calls, [['setVolume', 0]], `${start} - ${step}`)
	}
})

test('setVolume coerces before reaching the backend', async () => {
	const backend = fakeBackend()
	const c = createVolumeController({ backend, audio: audio() })
	await c.setVolume(33.7)
	await c.setVolume(-3)
	await c.setVolume('x')
	assert.deepEqual(backend.calls, [
		['setVolume', 34],
		['setVolume', 0],
		['setVolume', 0],
	])
})

test('mute / unmute / toggleMute / isMuted', async () => {
	const backend = fakeBackend()
	const c = createVolumeController({ backend, audio: audio() })
	await c.mute()
	assert.equal(await c.isMuted(), true)
	await c.toggleMute()
	assert.equal(await c.isMuted(), false)
	await c.toggleMute()
	assert.equal(await c.isMuted(), true)
	await c.unmute()
	assert.equal(await c.isMuted(), false)
})

test('getCapabilities: everything new is unsupported unless the backend says otherwise', async () => {
	const none = { protocolVersion: 2, outputDevices: { list: false, select: false }, apps: { volume: false, mute: false } }
	assert.deepEqual(await createVolumeController({ backend: fakeBackend(), audio: audio() }).getCapabilities(), none)

	const backend = { ...fakeBackend(), getCapabilities: async () => ({ apps: { volume: true, mute: 'yes' }, rogue: true, protocolVersion: 99 }) }
	assert.deepEqual(await createVolumeController({ backend, audio: audio() }).getCapabilities(), { ...none, apps: { volume: true, mute: false } })
})

test('getCapabilities asks the backend each time, since the environment can change', async () => {
	let pulse = false
	const backend = { ...fakeBackend(), getCapabilities: () => ({ outputDevices: { list: pulse } }) }
	const c = createVolumeController({ backend, audio: audio() })
	assert.equal((await c.getCapabilities()).outputDevices.list, false)
	pulse = true
	assert.equal((await c.getCapabilities()).outputDevices.list, true)
})

test('device name is cached so bursts do not block the thread', async () => {
	let t = 0
	const counter = { n: 0 }
	const c = createVolumeController({ backend: fakeBackend(), audio: audio(counter), deviceCacheMs: 1000, now: () => t })
	for (let i = 0; i < 50; i++) assert.equal(await c.getAudioDevice(), 'Speakers (HyperX Cloud II)')
	assert.equal(counter.n, 1)
	t = 999
	await c.getAudioDevice()
	assert.equal(counter.n, 1)
	t = 1000
	await c.getAudioDevice()
	assert.equal(counter.n, 2)
})
