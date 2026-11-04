import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

const execFileP = promisify(execFile)

async function osascript(cmd) {
	return (await execFileP('osascript', ['-e', cmd], { timeout: 5000 })).stdout.trim()
}

// Values are interpolated into AppleScript, so force them to a safe integer first.
const toVolume = (val) => Math.min(Math.max(Math.round(Number(val)) || 0, 0), 100)

export async function getVolume() {
	return parseInt(await osascript('output volume of (get volume settings)'), 10)
}

export async function setVolume(val) {
	await osascript(`set volume output volume ${toVolume(val)}`)
}

export async function getMuted() {
	return (await osascript('output muted of (get volume settings)')) === 'true'
}

export async function setMuted(val) {
	await osascript(`set volume ${val ? 'with' : 'without'} output muted`)
}
