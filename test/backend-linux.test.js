import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createLinuxBackend, parseDefaultDevice, parseInfo } from '../impl/linux.js'
import { fakeExecFile } from './helpers/fake-exec.js'

// Real `amixer` output samples.
const AMIXER_LIST_PULSE = `Simple mixer control 'Master',0
  Capabilities: pvolume pswitch pswitch-joined
  Playback channels: Front Left - Front Right
  Limits: Playback 0 - 65536
  Mono:
  Front Left: Playback 32768 [50%] [on]
  Front Right: Playback 32768 [50%] [on]
Simple mixer control 'Capture',0
  Capabilities: cvolume cswitch cswitch-joined
`

const AMIXER_GET_ALSA_DB = `Simple mixer control 'Master',0
  Capabilities: pvolume pvolume-joined pswitch pswitch-joined
  Playback channels: Mono
  Limits: Playback 0 - 87
  Mono: Playback 65 [75%] [-16.50dB] [off]
`

const AMIXER_GET_PCM = `Simple mixer control 'PCM',0
  Capabilities: pvolume pswitch
  Playback channels: Front Left - Front Right
  Limits: Playback 0 - 255
  Mono:
  Front Left: Playback 255 [100%] [0.00dB] [on]
  Front Right: Playback 255 [100%] [0.00dB] [on]
`

test('parseDefaultDevice picks the first simple mixer control', () => {
	assert.equal(parseDefaultDevice(AMIXER_LIST_PULSE), 'Master')
	assert.equal(parseDefaultDevice(AMIXER_GET_PCM), 'PCM')
	assert.throws(() => parseDefaultDevice('amixer: Mixer attach default error: No such file or directory'), /failed to parse/)
})

test('parseInfo handles PulseAudio and ALSA (with dB) output', () => {
	assert.deepEqual(parseInfo(AMIXER_LIST_PULSE), { volume: 50, muted: false })
	assert.deepEqual(parseInfo(AMIXER_GET_ALSA_DB), { volume: 75, muted: true })
	assert.deepEqual(parseInfo(AMIXER_GET_PCM), { volume: 100, muted: false })
	assert.throws(() => parseInfo('Simple mixer control \'Capture\',0\n  Capabilities: cvolume'), /failed to parse/)
})

test('backend discovers the control once and issues amixer commands', async () => {
	const exec = fakeExecFile((_file, args) => (args.length === 0 ? AMIXER_LIST_PULSE : AMIXER_GET_ALSA_DB))
	const b = createLinuxBackend({ execFile: exec })

	assert.equal(await b.getVolume(), 75)
	assert.equal(await b.getMuted(), true)
	await b.setVolume(42)
	await b.setVolume('9; reboot')
	await b.setMuted(true)
	await b.setMuted(false)

	assert.ok(exec.calls.every((c) => c.file === 'amixer'))
	assert.deepEqual(
		exec.calls.map((c) => c.args),
		[
			[],
			['get', 'Master'],
			['get', 'Master'],
			['set', 'Master', '42%'],
			['set', 'Master', '0%'],
			['set', 'Master', 'mute'],
			['set', 'Master', 'unmute'],
		],
	)
})

test('backend surfaces a missing amixer', async () => {
	const b = createLinuxBackend({
		execFile: fakeExecFile(() => {
			throw Object.assign(new Error('spawn amixer ENOENT'), { code: 'ENOENT' })
		}),
	})
	await assert.rejects(b.getVolume(), /ENOENT/)
})
