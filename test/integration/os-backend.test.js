// Drives the REAL audio backend of the current OS (Windows helper .exe, osascript or amixer).
// Opt-in because it briefly changes the system volume and mute state (both are restored):
//   RV_INTEGRATION=1 npm run test:integration
import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import os from 'node:os'
import { backendModuleFor } from '../../lib/volume.js'

const enabled = process.env.RV_INTEGRATION === '1'
const skip = enabled ? false : 'set RV_INTEGRATION=1 to run against the real audio device'

let backend
let original

before(async () => {
	if (!enabled) return
	backend = await import(new URL(backendModuleFor(os.type()), new URL('../../lib/', import.meta.url)).href)
	original = { volume: await backend.getVolume(), muted: await backend.getMuted() }
})

after(async () => {
	if (!enabled || !original) return
	await backend.setVolume(original.volume)
	await backend.setMuted(original.muted)
	assert.equal(await backend.getVolume(), original.volume, 'volume restored')
	assert.equal(await backend.getMuted(), original.muted, 'mute state restored')
})

test(`reads volume and mute state on ${os.type()}`, { skip }, async () => {
	assert.ok(Number.isInteger(original.volume) && original.volume >= 0 && original.volume <= 100, String(original.volume))
	assert.equal(typeof original.muted, 'boolean')
})

test(`sets the volume on ${os.type()}`, { skip }, async () => {
	const target = original.volume >= 50 ? original.volume - 2 : original.volume + 2
	await backend.setVolume(target)
	// Some drivers quantise volume steps; allow 1 % of rounding.
	assert.ok(Math.abs((await backend.getVolume()) - target) <= 1)
	await backend.setVolume(original.volume)
	assert.ok(Math.abs((await backend.getVolume()) - original.volume) <= 1)
})

test(`mutes and unmutes on ${os.type()}`, { skip }, async () => {
	await backend.setMuted(!original.muted)
	assert.equal(await backend.getMuted(), !original.muted)
	await backend.setMuted(original.muted)
	assert.equal(await backend.getMuted(), original.muted)
})
