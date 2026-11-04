// Wires the OS-specific backend and naudiodon into the volume controller used by the app.
import os from 'node:os'
import { getDevices, getHostAPIs } from '@capaj/naudiodon'
import { backendModuleFor, createVolumeController } from './lib/volume.js'

const backend = await import(new URL(backendModuleFor(os.type()), new URL('./lib/', import.meta.url)).href)

export const controller = createVolumeController({ backend, audio: { getDevices, getHostAPIs } })

export const {
	getAudioDevice,
	setVolume,
	getVolume,
	mute,
	unmute,
	toggleMute,
	isMuted,
	increaseVolume,
	decreaseVolume,
} = controller
