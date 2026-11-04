import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFileSync, existsSync } from 'node:fs'
import {
	createWindowsBackend,
	parseVolumeInfo,
	resolveHelperPath,
	volumeArg,
	HELPER_PATH,
	HELPER_ZERO_ARG,
} from '../impl/windows/index.js'
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

test('parseVolumeInfo explains helper COM errors', () => {
	// Real output from a GitHub-hosted Windows runner (no audio endpoint): -0x80070490 as signed int.
	assert.throws(() => parseVolumeInfo('error 2147023728 occurred\r\n'), /HRESULT 0x80070490 \(no default audio output device\)/)
	assert.throws(() => parseVolumeInfo('error -2147023728 occurred'), /HRESULT 0x80070490/)
	// E_FAIL (0x80004005) in both encodings, no known description.
	for (const out of ['error 2147467259 occurred', 'error 2147500037 occurred']) {
		assert.throws(() => parseVolumeInfo(out), (e) => /HRESULT 0x80004005$/.test(e.message), out)
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
	await b.setVolume(1)
	await b.setMuted(true)
	await b.setMuted(false)

	assert.ok(exec.calls.every((c) => c.file === 'helper.exe'))
	assert.ok(exec.calls.every((c) => c.options.windowsHide === true && c.options.timeout > 0))
	assert.deepEqual(
		exec.calls.map((c) => c.args),
		[[], [], ['42'], ['51'], ['100'], [HELPER_ZERO_ARG], ['1'], ['mute'], ['unmute']],
	)
})

test('volume 0 is sent in a form the helper does not ignore', async () => {
	// Regression: the helper silently ignores "0", so decreasing to 0 never reached 0.
	assert.equal(HELPER_ZERO_ARG, '1e-9')
	for (const zero of [0, -0, 0.3, -10, '0']) assert.equal(volumeArg(zero), HELPER_ZERO_ARG, String(zero))
	assert.notEqual(Number(HELPER_ZERO_ARG), 0)
	assert.ok(Number(HELPER_ZERO_ARG) < 0.5, 'still rounds to 0 % when read back')
	assert.equal(volumeArg(0.6), '1')
	assert.equal(volumeArg(100), '100')

	const exec = fakeExecFile(() => '')
	await createWindowsBackend({ execFile: exec, helperPath: 'helper.exe' }).setVolume(0)
	assert.deepEqual(exec.calls[0].args, ['1e-9'])
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
