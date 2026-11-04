import { test } from 'node:test'
import assert from 'node:assert/strict'
import { sanitizeConfig, DEFAULT_CONFIG, CONFIG_VERSION } from '../lib/config.js'

test('defaults when nothing stored', () => {
	const c = sanitizeConfig(undefined)
	assert.deepEqual(c, { ...DEFAULT_CONFIG, websocket: { ...DEFAULT_CONFIG.websocket, allowedOrigins: [] }, polling: { ...DEFAULT_CONFIG.polling } })
})

test('migrates legacy 1.0.0 config (no host/token)', () => {
	const c = sanitizeConfig({ websocket: { port: 3000 }, runOnStartup: true, polling: { enabled: true, interval: 250 }, version: '1.0.0' })
	assert.equal(c.websocket.port, 3000)
	assert.equal(c.websocket.host, '0.0.0.0')
	assert.equal(c.websocket.token, '')
	assert.equal(c.runOnStartup, true)
	assert.deepEqual(c.polling, { enabled: true, interval: 250 })
	assert.equal(c.version, CONFIG_VERSION)
})

test('rejects hostile / malformed values from IPC', () => {
	const prev = sanitizeConfig({ websocket: { port: 4000 } })
	const c = sanitizeConfig(
		{
			websocket: { port: 'abc', host: '10.0.0.1', token: 'has space', allowedOrigins: ['javascript:alert(1)', 'https://ok.example'] },
			runOnStartup: 'yes',
			polling: { enabled: 1, interval: 1 },
			__proto__: { polluted: true },
			extra: 'dropped',
		},
		prev,
	)
	assert.equal(c.websocket.port, 4000)
	assert.equal(c.websocket.host, '0.0.0.0')
	assert.equal(c.websocket.token, '')
	assert.deepEqual(c.websocket.allowedOrigins, ['https://ok.example'])
	assert.equal(c.runOnStartup, false)
	assert.equal(c.polling.enabled, false)
	assert.equal(c.polling.interval, 100)
	assert.equal('extra' in c, false)
	assert.equal({}.polluted, undefined)
})

test('clamps port and interval', () => {
	assert.equal(sanitizeConfig({ websocket: { port: 70000 } }).websocket.port, 65535)
	assert.equal(sanitizeConfig({ polling: { interval: 99999 } }).polling.interval, 10000)
})
