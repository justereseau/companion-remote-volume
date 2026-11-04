import { execFile as nodeExecFile } from 'node:child_process'
import { runFile, toVolume } from '../lib/exec.js'

/** Parse `output volume of (get volume settings)`. Devices without volume control return "missing value". */
export function parseVolume(stdout) {
	const data = String(stdout).trim()
	if (!/^\d{1,3}$/.test(data) || Number(data) > 100) {
		throw new Error(`Output device does not report a volume (osascript returned ${JSON.stringify(data.slice(0, 64))})`)
	}
	return Number(data)
}

/** Parse `output muted of (get volume settings)`. */
export function parseMuted(stdout) {
	const data = String(stdout).trim()
	if (data === 'true') return true
	if (data === 'false') return false
	throw new Error(`Output device does not report a mute state (osascript returned ${JSON.stringify(data.slice(0, 64))})`)
}

export function createDarwinBackend({ execFile = nodeExecFile } = {}) {
	const osascript = (script) => runFile(execFile, 'osascript', ['-e', script])

	return {
		getVolume: async () => parseVolume(await osascript('output volume of (get volume settings)')),
		getMuted: async () => parseMuted(await osascript('output muted of (get volume settings)')),
		// Values are interpolated into AppleScript, so they are forced to a safe integer first.
		setVolume: async (val) => {
			await osascript(`set volume output volume ${toVolume(val)}`)
		},
		setMuted: async (val) => {
			await osascript(`set volume ${val ? 'with' : 'without'} output muted`)
		},
	}
}

export const { getVolume, setVolume, getMuted, setMuted } = createDarwinBackend()
