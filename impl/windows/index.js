import { execFile as nodeExecFile } from 'node:child_process'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { runFile, toVolume } from '../../lib/exec.js'

const __dirname = dirname(fileURLToPath(import.meta.url))

/**
 * In packaged builds the helper lives in app.asar.unpacked (see "asarUnpack" in package.json),
 * so it runs from the install directory instead of being extracted to a temp folder at runtime.
 */
export function resolveHelperPath(path) {
	return path.replace(/app\.asar([\\/])/, 'app.asar.unpacked$1')
}

// Helper binary from https://github.com/LinusU/node-loudness (SHA-256
// 9F0E5F9A6600D28A687460A46828C068E5A00C6E43FC4F6A6453509A72E6DDB4, identical to loudness@0.4.2).
// Usage: no args -> prints "<volume> <muted 0|1>"; "<0-100>" sets volume; "mute" / "unmute".
export const HELPER_PATH = resolveHelperPath(join(__dirname, 'adjust_get_current_system_volume_vista_plus.exe'))

const KNOWN_HRESULTS = Object.freeze({
	0x80070490: 'no default audio output device',
})

/** Parse the helper's "<volume> <muted>" output. */
export function parseVolumeInfo(stdout) {
	const data = String(stdout).trim()
	// On COM failures the helper prints "error <n> occurred" where n is the magnitude of the
	// (negative, signed 32-bit) HRESULT, e.g. 2147023728 for 0x80070490.
	const failure = /^error (-?\d+) occurred$/.exec(data)
	if (failure) {
		const n = Math.abs(Number(failure[1]))
		const code = n >= 0x80000000 ? n : (0x100000000 - n) >>> 0
		const hex = `0x${code.toString(16).toUpperCase().padStart(8, '0')}`
		throw new Error(`Volume helper failed with HRESULT ${hex}${KNOWN_HRESULTS[code] ? ` (${KNOWN_HRESULTS[code]})` : ''}`)
	}
	const match = /^(\d{1,3})\s+([01])$/.exec(data)
	if (!match || Number(match[1]) > 100) {
		throw new Error(`Unexpected output from volume helper: ${JSON.stringify(data.slice(0, 64))}`)
	}
	return { volume: Number(match[1]), muted: match[2] === '1' }
}

/**
 * The helper treats a numeric argument of 0 ("0", "0.0", "00") as "no value" and silently leaves
 * the volume unchanged. "1e-9" is parsed as a number > 0 but lands on scalar 0.0 (the endpoint's
 * minimum, e.g. -74 dB), measured identical to IAudioEndpointVolume::SetMasterVolumeLevelScalar(0).
 */
export const HELPER_ZERO_ARG = '1e-9'

/** Command-line argument for a volume in 0..100. */
export function volumeArg(val) {
	const v = toVolume(val)
	return v === 0 ? HELPER_ZERO_ARG : String(v)
}

export function createWindowsBackend({ execFile = nodeExecFile, helperPath = HELPER_PATH } = {}) {
	const run = (...args) => runFile(execFile, helperPath, args)
	const info = async () => parseVolumeInfo(await run())

	return {
		getVolume: async () => (await info()).volume,
		getMuted: async () => (await info()).muted,
		setVolume: async (val) => {
			await run(volumeArg(val))
		},
		setMuted: async (val) => {
			await run(val ? 'mute' : 'unmute')
		},
	}
}

export const { getVolume, setVolume, getMuted, setMuted } = createWindowsBackend()
