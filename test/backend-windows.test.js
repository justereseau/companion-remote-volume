import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFileSync, existsSync } from 'node:fs'
import { createWindowsBackend, parseVolumeInfo, resolveHelperPath, HELPER_PATH } from '../impl/windows/index.js'
import { fakeExecFile } from './helpers/fake-exec.js'

const HELPER_SHA256 = '9f0e5f9a6600d28a687460a46828c068e5a00c6e43fc4f6a6453509a72e6ddb4'

test('bundled helper exists and matches the pinned loudness@0.4.2 binary', () => {
	assert.ok(existsSync(HELPER_PATH), HELPER_PATH)
	const sha = createHash('sha256').update(readFileSync(HELPER_PATH)).digest('hex')
	assert.equal(sha, HELPER_SHA256, 'helper binary changed – verify its origin before updating this hash')
})

test('resolveHelperPath points into app.asar.unpacked in packaged builds', () => {
	assert.equal(
		resolveHelperPath('C:\\Program Files\\Remote Volume\\resources\\app.asar\\impl\\windows\\x.exe'),
		'C:\\Program Files\\Remote Volume\\resources\\app.asar.unpacked\\impl\\windows\\x.exe',
	)
	assert.equal(resolveHelperPath('/opt/rv/resources/app.asar/impl/windows/x.exe'), '/opt/rv/resources/app.asar.unpacked/impl/windows/x.exe')
	assert.equal(resolveHelperPath('C:\\src\\rv\\impl\\windows\\x.exe'), 'C:\\src\\rv\\impl\\windows\\x.exe')
	// Already unpacked paths are left alone.
	assert.equal(resolveHelperPath('/r/app.asar.unpacked/x.exe'), '/r/app.asar.unpacked/x.exe')
})

test('parseVolumeInfo accepts helper output', () => {
	assert.deepEqual(parseVolumeInfo('14 0'), { volume: 14, muted: false })
	assert.deepEqual(parseVolumeInfo('100 1\r\n'), { volume: 100, muted: true })
	assert.deepEqual(parseVolumeInfo('0 0\n'), { volume: 0, muted: false })
})

test('parseVolumeInfo rejects unexpected output', () => {
	for (const bad of ['', '14', 'abc def', '101 0', '14 2', '-1 0', '14 0 extra', 'Error: no device']) {
		assert.throws(() => parseVolumeInfo(bad), /Unexpected output/, JSON.stringify(bad))
	}
})

test('backend invokes the helper with the documented arguments', async () => {
	const exec = fakeExecFile(() => '37 1')
	const b = createWindowsBackend({ execFile: exec, helperPath: 'helper.exe' })

	assert.equal(await b.getVolume(), 37)
	assert.equal(await b.getMuted(), true)
	await b.setVolume(42)
	await b.setVolume(50.6)
	await b.setVolume(500)
	await b.setVolume('7 & calc.exe')
	await b.setMuted(true)
	await b.setMuted(false)

	assert.ok(exec.calls.every((c) => c.file === 'helper.exe'))
	assert.ok(exec.calls.every((c) => c.options.windowsHide === true && c.options.timeout > 0))
	assert.deepEqual(
		exec.calls.map((c) => c.args),
		[[], [], ['42'], ['51'], ['100'], ['0'], ['mute'], ['unmute']],
	)
})

test('backend surfaces helper failures', async () => {
	const b = createWindowsBackend({
		execFile: fakeExecFile(() => {
			throw new Error('spawn helper.exe ENOENT')
		}),
		helperPath: 'helper.exe',
	})
	await assert.rejects(b.getVolume(), /ENOENT/)
	const garbage = createWindowsBackend({ execFile: fakeExecFile(() => 'garbage'), helperPath: 'helper.exe' })
	await assert.rejects(garbage.getMuted(), /Unexpected output/)
})
