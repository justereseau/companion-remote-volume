import os from 'node:os'
import { getDevices, getHostAPIs } from '@capaj/naudiodon'
let impl = null

switch (os.type()) {
	case 'Darwin':
		impl = await import('./impl/darwin.js')
		break
	case 'Linux':
		impl = await import('./impl/linux.js')
		break
	case 'Windows_NT':
		impl = await import('./impl/windows/index.js')
		break
	default:
		throw new Error('Your OS is currently not supported by node-loudness.')
}

export async function getAudioDevice() {
	const hostAPIs = getHostAPIs()
	const defaultOutput = hostAPIs?.HostAPIs?.[hostAPIs.defaultHostAPI]?.defaultOutput
	// PortAudio reports -1 when there is no output device (e.g. headset unplugged).
	if (!Number.isInteger(defaultOutput) || defaultOutput < 0) return null

	const device = getDevices().find((d) => d.id === defaultOutput)
	return device?.name ?? null
}

const clampVolume = (value) => Math.min(Math.max(Math.round(Number(value)), 0), 100)

export async function setVolume(value) {
	return impl.setVolume(clampVolume(value))
}

export async function getVolume() {
	return impl.getVolume()
}

export async function mute() {
	return impl.setMuted(true)
}

export async function unmute() {
	return impl.setMuted(false)
}

export async function toggleMute() {
	const currentlyMuted = await impl.getMuted()
	return impl.setMuted(!currentlyMuted)
}

export async function isMuted() {
	return impl.getMuted()
}

export async function increaseVolume(amount) {
	const currentVolume = await impl.getVolume()
	return impl.setVolume(clampVolume(currentVolume + amount))
}

export async function decreaseVolume(amount) {
	const currentVolume = await impl.getVolume()
	return impl.setVolume(clampVolume(currentVolume - amount))
}
