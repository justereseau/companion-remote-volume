import { execFile as nodeExecFile } from 'node:child_process'
import { runFile, toVolume } from '../lib/exec.js'

const reDefaultDevice = /Simple mixer control '([a-z0-9 -]+)',[0-9]+/i

/** First simple mixer control listed by `amixer` (e.g. "Master"). */
export function parseDefaultDevice(stdout) {
	const result = reDefaultDevice.exec(stdout)
	if (result === null) throw new Error('Alsa Mixer Error: failed to parse output')
	return result[1]
}

const reInfo = /[a-z][a-z ]*: Playback [0-9-]+ \[([0-9]+)%\] (?:\[[0-9.-]+dB\] )?\[(on|off)\]/i

/** Volume and mute state from `amixer get <control>`. */
export function parseInfo(stdout) {
	const result = reInfo.exec(stdout)
	if (result === null) throw new Error('Alsa Mixer Error: failed to parse output')
	return { volume: parseInt(result[1], 10), muted: result[2] === 'off' }
}

export function createLinuxBackend({ execFile = nodeExecFile } = {}) {
	const amixer = (...args) => runFile(execFile, 'amixer', args)
	let defaultDevice = null
	const getDefaultDevice = async () => (defaultDevice ??= parseDefaultDevice(await amixer()))
	const info = async () => parseInfo(await amixer('get', await getDefaultDevice()))

	return {
		getVolume: async () => (await info()).volume,
		getMuted: async () => (await info()).muted,
		setVolume: async (val) => {
			await amixer('set', await getDefaultDevice(), `${toVolume(val)}%`)
		},
		setMuted: async (val) => {
			await amixer('set', await getDefaultDevice(), val ? 'mute' : 'unmute')
		},
	}
}

export const { getVolume, setVolume, getMuted, setMuted } = createLinuxBackend()
