// OS-independent volume logic on top of a platform backend
// ({ getVolume, setVolume, getMuted, setMuted }) and naudiodon's device enumeration.
import { toVolume } from './exec.js'

/** Backend module for each `os.type()` value. */
export const BACKEND_MODULES = Object.freeze({
	Darwin: '../impl/darwin.js',
	Linux: '../impl/linux.js',
	Windows_NT: '../impl/windows/index.js',
})

export function backendModuleFor(osType) {
	if (!Object.hasOwn(BACKEND_MODULES, osType)) throw new Error(`Your OS (${osType}) is currently not supported.`)
	return BACKEND_MODULES[osType]
}

/** Name of the default output device from naudiodon's getHostAPIs()/getDevices(), or null. */
export function defaultOutputName(hostAPIs, devices) {
	const defaultOutput = hostAPIs?.HostAPIs?.[hostAPIs.defaultHostAPI]?.defaultOutput
	// PortAudio reports -1 when there is no output device (e.g. headset unplugged).
	if (!Number.isInteger(defaultOutput) || defaultOutput < 0) return null
	return devices?.find((d) => d.id === defaultOutput)?.name ?? null
}

/**
 * @param {object} deps
 * @param {{getVolume, setVolume, getMuted, setMuted}} deps.backend
 * @param {{getDevices: () => any[], getHostAPIs: () => any}} deps.audio naudiodon (synchronous, blocks the thread)
 * @param {number} [deps.deviceCacheMs] how long to reuse the device name
 * @param {() => number} [deps.now]
 */
export function createVolumeController({ backend, audio, deviceCacheMs = 1000, now = () => Date.now() }) {
	// naudiodon enumerates devices with synchronous PortAudio calls that block the Electron main
	// thread for tens of milliseconds. Caching bounds that cost to one enumeration per interval.
	let deviceCache = { at: -Infinity, name: null }

	const controller = {
		async getAudioDevice() {
			const t = now()
			if (t - deviceCache.at >= deviceCacheMs) {
				deviceCache = { at: t, name: defaultOutputName(audio.getHostAPIs(), audio.getDevices()) }
			}
			return deviceCache.name
		},
		getVolume: () => backend.getVolume(),
		setVolume: (value) => backend.setVolume(toVolume(value)),
		isMuted: () => backend.getMuted(),
		mute: () => backend.setMuted(true),
		unmute: () => backend.setMuted(false),
		async toggleMute() {
			return backend.setMuted(!(await backend.getMuted()))
		},
		async increaseVolume(amount) {
			return backend.setVolume(toVolume((await backend.getVolume()) + amount))
		},
		async decreaseVolume(amount) {
			return backend.setVolume(toVolume((await backend.getVolume()) - amount))
		},
	}
	return controller
}
