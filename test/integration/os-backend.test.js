// Drives the REAL audio backend of the current OS (Windows helper .exe, osascript or amixer).
// Opt-in because it briefly changes the system volume and mute state (both are restored):
//   RV_INTEGRATION=1 npm run test:integration
// On machines without any audio output device (e.g. hosted Windows CI runners):
//   RV_INTEGRATION=no-device npm run test:integration
import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import os from 'node:os'
import { backendModuleFor } from '../../lib/volume.js'

const mode = process.env.RV_INTEGRATION
const full = mode === '1'
const noDevice = mode === 'no-device'
const skipFull = full ? false : 'set RV_INTEGRATION=1 to run against the real audio device'
const skipNoDevice = noDevice ? false : 'set RV_INTEGRATION=no-device on a machine without audio output'

let backend
let original

before(async () => {
	if (!full && !noDevice) return
	backend = await import(new URL(backendModuleFor(os.type()), new URL('../../lib/', import.meta.url)).href)
	if (full) original = { volume: await backend.getVolume(), muted: await backend.getMuted() }
})

after(async () => {
	if (!full || !original) return
	await backend.setVolume(original.volume)
	await backend.setMuted(original.muted)
	assert.equal(await backend.getVolume(), original.volume, 'volume restored')
	assert.equal(await backend.getMuted(), original.muted, 'mute state restored')
})

test(`reads volume and mute state on ${os.type()}`, { skip: skipFull }, async () => {
	assert.ok(Number.isInteger(original.volume) && original.volume >= 0 && original.volume <= 100, String(original.volume))
	assert.equal(typeof original.muted, 'boolean')
})

test(`sets the volume on ${os.type()}`, { skip: skipFull }, async () => {
	const target = original.volume >= 50 ? original.volume - 2 : original.volume + 2
	await backend.setVolume(target)
	// Some drivers quantise volume steps; allow 1 % of rounding.
	assert.ok(Math.abs((await backend.getVolume()) - target) <= 1)
	await backend.setVolume(original.volume)
	assert.ok(Math.abs((await backend.getVolume()) - original.volume) <= 1)
})

test(`sets the volume to exactly 0 and back on ${os.type()}`, { skip: skipFull }, async () => {
	// Regression: the Windows helper silently ignored "0".
	await backend.setVolume(1)
	assert.ok((await backend.getVolume()) <= 2)
	await backend.setVolume(0)
	assert.equal(await backend.getVolume(), 0)
	await backend.setVolume(original.volume)
	assert.ok(Math.abs((await backend.getVolume()) - original.volume) <= 1)
})

test(`mutes and unmutes on ${os.type()}`, { skip: skipFull }, async () => {
	await backend.setMuted(!original.muted)
	assert.equal(await backend.getMuted(), !original.muted)
	await backend.setMuted(original.muted)
	assert.equal(await backend.getMuted(), original.muted)
})

test(`reports a missing audio device clearly on ${os.type()}`, { skip: skipNoDevice }, async () => {
	// The OS tool must actually run (binary present, executable, right architecture) and the
	// failure must surface as a descriptive error rather than NaN or a crash.
	await assert.rejects(backend.getVolume(), (error) => {
		assert.match(error.message, /no default audio output device|does not report|failed to parse/i)
		return true
	})
	await assert.rejects(backend.getMuted())
})
