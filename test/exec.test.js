import { test } from 'node:test'
import assert from 'node:assert/strict'
import { runFile, toVolume, EXEC_TIMEOUT_MS } from '../lib/exec.js'
import { fakeExecFile } from './helpers/fake-exec.js'

test('runFile resolves stdout and applies timeout + windowsHide', async () => {
	const exec = fakeExecFile(() => 'out')
	assert.equal(await runFile(exec, 'tool', ['a', 'b']), 'out')
	assert.deepEqual(exec.calls[0].args, ['a', 'b'])
	assert.equal(exec.calls[0].options.timeout, EXEC_TIMEOUT_MS)
	assert.equal(exec.calls[0].options.windowsHide, true)
})

test('runFile rejects when the process fails', async () => {
	const exec = fakeExecFile(() => {
		throw new Error('ENOENT')
	})
	await assert.rejects(runFile(exec, 'missing', []), /ENOENT/)
})

test('runFile really spawns a process (current OS)', async () => {
	const out = await runFile(undefined, process.execPath, ['-e', 'process.stdout.write("hi")'])
	assert.equal(out, 'hi')
})

test('toVolume clamps, rounds and neutralises non-numbers', () => {
	const cases = [
		[50, 50],
		[50.4, 50],
		[50.5, 51],
		[-5, 0],
		[150, 100],
		['42', 42],
		['1; rm -rf /', 0],
		['50 do shell script "x"', 0],
		[NaN, 0],
		[Infinity, 0],
		[null, 0],
		[undefined, 0],
	]
	for (const [input, expected] of cases) assert.equal(toVolume(input), expected, `toVolume(${JSON.stringify(input)})`)
})
