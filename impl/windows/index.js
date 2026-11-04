import { execFile } from 'node:child_process'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'

const __dirname = dirname(fileURLToPath(import.meta.url))

// Helper binary from https://github.com/LinusU/node-loudness (SHA-256
// 9F0E5F9A6600D28A687460A46828C068E5A00C6E43FC4F6A6453509A72E6DDB4, identical to loudness@0.4.2).
// In packaged builds it is unpacked from app.asar (see "asarUnpack" in package.json) so that it is
// executed from the install directory instead of being extracted to a temp folder at runtime.
const executablePath = join(__dirname, 'adjust_get_current_system_volume_vista_plus.exe').replace(
	/app\.asar([\\/])/,
	'app.asar.unpacked$1',
)
const execFilePromisified = promisify(execFile)

async function runProgram(...args) {
	const { stdout } = await execFilePromisified(executablePath, args, { timeout: 5000, windowsHide: true })
	return stdout
}

async function getVolumeInfo() {
	const data = (await runProgram()).trim()
	const [volume, muted] = data.split(/\s+/).map((v) => parseInt(v, 10))
	if (!Number.isInteger(volume) || !Number.isInteger(muted)) {
		throw new Error(`Unexpected output from volume helper: ${JSON.stringify(data.slice(0, 64))}`)
	}
	return { volume, muted: Boolean(muted) }
}

export async function getVolume() {
	return (await getVolumeInfo()).volume
}

export async function setVolume(val) {
	await runProgram(String(val))
}

export async function getMuted() {
	return (await getVolumeInfo()).muted
}

export async function setMuted(val) {
	await runProgram(val ? 'mute' : 'unmute')
}
