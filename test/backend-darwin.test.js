import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createDarwinBackend, parseVolume, parseMuted } from '../impl/darwin.js'
import { fakeExecFile } from './helpers/fake-exec.js'

test('parseVolume / parseMuted accept osascript output', () => {
	assert.equal(parseVolume('44\n'), 44)
	assert.equal(parseVolume('0'), 0)
	assert.equal(parseVolume('100'), 100)
	assert.equal(parseMuted('true\n'), true)
	assert.equal(parseMuted('false'), false)
})

test('devices without volume control ("missing value") raise a clear error', () => {
	assert.throws(() => parseVolume('missing value'), /does not report a volume/)
	assert.throws(() => parseVolume(''), /does not report a volume/)
	assert.throws(() => parseVolume('101'), /does not report a volume/)
	assert.throws(() => parseMuted('missing value'), /does not report a mute state/)
})

test('backend runs the expected AppleScript', async () => {
	const exec = fakeExecFile((_file, [, script]) => (script.startsWith('output muted') ? 'false' : '25'))
	const b = createDarwinBackend({ execFile: exec })

	assert.equal(await b.getVolume(), 25)
	assert.equal(await b.getMuted(), false)
	await b.setVolume(30)
	await b.setMuted(true)
	await b.setMuted(false)

	assert.ok(exec.calls.every((c) => c.file === 'osascript' && c.args[0] === '-e' && c.args.length === 2))
	assert.deepEqual(
		exec.calls.map((c) => c.args[1]),
		[
			'output volume of (get volume settings)',
			'output muted of (get volume settings)',
			'set volume output volume 30',
			'set volume with output muted',
			'set volume without output muted',
		],
	)
})

test('setVolume cannot inject AppleScript', async () => {
	const exec = fakeExecFile(() => '')
	const b = createDarwinBackend({ execFile: exec })
	await b.setVolume('5\ndo shell script "touch /tmp/pwned"')
	await b.setVolume({ toString: () => '1 & do shell script "id"' })
	await b.setVolume(73.2)
	assert.deepEqual(
		exec.calls.map((c) => c.args[1]),
		['set volume output volume 0', 'set volume output volume 0', 'set volume output volume 73'],
	)
})

test('backend surfaces osascript failures', async () => {
	const b = createDarwinBackend({
		execFile: fakeExecFile(() => {
			throw new Error('osascript: execution error')
		}),
	})
	await assert.rejects(b.getVolume(), /execution error/)
})
