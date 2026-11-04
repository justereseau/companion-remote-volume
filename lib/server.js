// WebSocket server: admission control, command dispatch, state polling and broadcast.
// Independent of Electron so it can be exercised end-to-end in plain Node tests.
import { WebSocketServer, WebSocket } from 'ws'
import { parseMessage, admitClient, RateLimiter, MAX_PAYLOAD_BYTES } from './protocol.js'

const noopLog = { info() {}, warn() {}, error() {} }

/**
 * @param {object} deps
 * @param {() => {websocket: {port, host, token, allowedOrigins}, polling: {enabled, interval}}} deps.getConfig
 * @param {ReturnType<import('./volume.js').createVolumeController>} deps.controller
 * @param {{info, warn, error}} [deps.log]
 * @param {() => RateLimiter} [deps.createRateLimiter]
 */
export function createVolumeServer({ getConfig, controller, log = noopLog, createRateLimiter = () => new RateLimiter() }) {
	let wss = null
	let pollTimer = null
	let pollInFlight = false
	let last = { volume: null, muted: null, outputDevice: null }

	async function readState() {
		const [volume, muted, outputDevice] = await Promise.all([
			controller.getVolume(),
			controller.isMuted(),
			controller.getAudioDevice(),
		])
		return { volume, muted, outputDevice }
	}

	const commands = {
		setVolume: (value) => controller.setVolume(value),
		increaseVolume: (value) => controller.increaseVolume(value),
		decreaseVolume: (value) => controller.decreaseVolume(value),
		mute: () => controller.mute(),
		unmute: () => controller.unmute(),
		toggleMute: () => controller.toggleMute(),
		getState: () => readState(),
		isMuted: async () => ({ muted: await controller.isMuted() }),
	}
	const REPLYING_ACTIONS = new Set(['getState', 'isMuted'])

	function broadcast(state) {
		if (!wss) return
		const payload = JSON.stringify(state)
		for (const client of wss.clients) {
			if (client.readyState === WebSocket.OPEN) client.send(payload)
		}
	}

	async function pollOnce() {
		// Skip a tick instead of piling up child processes when the OS is slow.
		if (pollInFlight) return
		pollInFlight = true
		try {
			const state = await readState()
			for (const key of ['volume', 'muted', 'outputDevice']) {
				if (state[key] !== last[key]) {
					last[key] = state[key]
					broadcast({ [key]: state[key] })
				}
			}
		} catch (error) {
			log.error('Error monitoring volume/mute state:', error)
		} finally {
			pollInFlight = false
		}
	}

	function startMonitoring() {
		stopMonitoring()
		const { polling } = getConfig()
		if (!polling.enabled) return
		log.info(`Starting volume/mute state monitoring every ${polling.interval} ms.`)
		pollTimer = setInterval(pollOnce, polling.interval)
	}

	function stopMonitoring() {
		if (pollTimer) clearInterval(pollTimer)
		pollTimer = null
	}

	function handleConnection(ws, req) {
		log.info(`Client connected from ${req.socket.remoteAddress}`)
		const limiter = createRateLimiter()
		const send = (obj) => {
			if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj))
		}

		ws.on('error', (error) => log.warn('WebSocket client error:', error.message))

		ws.on('message', async (raw, isBinary) => {
			if (isBinary) return
			if (!limiter.tryRemove()) return send({ error: 'Rate limit exceeded' })

			const msg = parseMessage(raw)
			if (!msg.ok) {
				log.warn(`Rejected client message: ${msg.error}`)
				return send(msg.action ? { action: msg.action, response: { error: msg.error } } : { error: msg.error })
			}

			try {
				const response = await commands[msg.action](msg.value)
				if (REPLYING_ACTIONS.has(msg.action)) send(response)
			} catch (error) {
				log.error(`Error executing ${msg.action}:`, error)
				send({ action: msg.action, response: { error: 'Failed to execute action' } })
			}
		})

		readState().then(send, (error) => log.error('Failed to send initial state:', error))
	}

	/** Start listening with the current config. Resolves with the bound address. */
	function start() {
		const { port, host } = getConfig().websocket
		return new Promise((resolve, reject) => {
			const server = new WebSocketServer({
				port,
				host,
				maxPayload: MAX_PAYLOAD_BYTES,
				perMessageDeflate: false,
				verifyClient: ({ req }, done) => {
					const verdict = admitClient(req, getConfig().websocket, server.clients.size)
					if (verdict.ok) return done(true)
					log.warn(`Rejected WebSocket client ${req.socket.remoteAddress}: ${verdict.reason}`)
					done(false, verdict.code, verdict.reason)
				},
			})
			wss = server

			server.once('listening', () => {
				const addr = server.address()
				log.info(`WebSocket server listening on ws://${addr.address}:${addr.port}${getConfig().websocket.token ? ' (token required)' : ''}`)
				startMonitoring()
				resolve(addr)
			})
			server.on('error', (error) => {
				log.error('WebSocket server error:', error)
				reject(error)
			})
			server.on('connection', handleConnection)
		})
	}

	/** Stop listening and disconnect every client. */
	function stop() {
		stopMonitoring()
		const old = wss
		wss = null
		if (!old) return Promise.resolve()
		for (const client of old.clients) client.terminate()
		return new Promise((resolve) => old.close(() => resolve()))
	}

	/** Apply a changed configuration (port, host, token, polling). */
	async function restart() {
		log.info('Restarting WebSocket server to apply new configuration.')
		await stop()
		last = { volume: null, muted: null, outputDevice: null }
		return start()
	}

	return {
		start,
		stop,
		restart,
		pollOnce,
		readState,
		get clientCount() {
			return wss ? wss.clients.size : 0
		},
	}
}
