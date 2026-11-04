import { execFile as nodeExecFile } from 'node:child_process'

/** Default timeout for helper processes (amixer, osascript, the Windows helper). */
export const EXEC_TIMEOUT_MS = 5000

/**
 * Promise wrapper around a callback-style `execFile` that resolves with stdout.
 * Taking `execFile` as a parameter lets tests substitute a fake process runner.
 */
export function runFile(execFile = nodeExecFile, file, args, options = {}) {
	return new Promise((resolve, reject) => {
		execFile(file, args, { timeout: EXEC_TIMEOUT_MS, windowsHide: true, ...options }, (error, stdout) => {
			if (error) reject(error)
			else resolve(String(stdout))
		})
	})
}

/** Coerce anything to an integer volume in [0, 100] before it reaches a command line or script. */
export function toVolume(value) {
	const n = Math.round(Number(value))
	return Number.isFinite(n) ? Math.min(Math.max(n, 0), 100) : 0
}
