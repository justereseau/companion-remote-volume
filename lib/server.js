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
	let refreshing = null
	let refreshAgain = false
	let last = { volume: null, muted: null, outputDevice: null }
	const STATE_KEYS = ['volume', 'muted', 'outputDevice']
	const handshakes = new WeakMap()

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
	const MUTATING_ACTIONS = new Set(['setVolume', 'increaseVolume', 'decreaseVolume', 'mute', 'unmute', 'toggleMute'])

	function broadcast(message, exclude = null) {
		if (!wss) return
		const payload = JSON.stringify(message)
		for (const client of wss.clients) {
			if (client !== exclude && client.readyState === WebSocket.OPEN) client.send(payload)
		}
	}

	/** Broadcast each field that differs from what clients were last told, one message per field. */
	function broadcastChanges(state, exclude = null) {
		for (const key of STATE_KEYS) {
			if (state[key] !== last[key]) {
				last[key] = state[key]
				broadcast({ [key]: state[key] }, exclude)
			}
		}
	}

	/**
	 * Read the OS state and broadcast what changed. Calls made while a refresh is running are
	 * coalesced into exactly one extra pass, so the latest change is never missed and a burst of
	 * commands does not spawn a burst of helper processes.
	 */
	function refresh() {
		if (refreshing) {
			refreshAgain = true
			return refreshing
		}
		refreshing = (async () => {
			try {
				do {
					refreshAgain = false
					try {
						broadcastChanges(await readState())
					} catch (error) {
						log.error('Error reading volume/mute state:', error)
					}
				} while (refreshAgain)
			} finally {
				refreshing = null
			}
		})()
		return refreshing
	}

	/** Periodic check for changes made outside the app (keyboard, OS mixer, other apps). */
	function startMonitoring() {
		stopMonitoring()
		const { polling } = getConfig()
		if (!polling.enabled) return
		log.info(`Starting volume/mute state monitoring every ${polling.interval} ms.`)
		// Skip a tick instead of piling up child processes when the OS is slow.
		pollTimer = setInterval(() => refreshing || refresh(), polling.interval)
	}

	function stopMonitoring() {
		if (pollTimer) clearInterval(pollTimer)
		pollTimer = null
	}

	function handleConnection(ws, req) {
		log.info(`Client connected from ${req.socket.remoteAddress}`)
		// Remember what the client presented so access-rule changes can be re-checked without a restart.
		handshakes.set(ws, {
			url: req.url,
			headers: { origin: req.headers.origin, authorization: req.headers.authorization },
			remoteAddress: req.socket.remoteAddress,
		})
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

			let response
			try {
				response = await commands[msg.action](msg.value)
			} catch (error) {
				log.error(`Error executing ${msg.action}:`, error)
				return send({ action: msg.action, response: { error: 'Failed to execute action' } })
			}
			if (REPLYING_ACTIONS.has(msg.action)) send(response)
			// Push the resulting state to every client right away, whether or not polling is enabled.
			if (MUTATING_ACTIONS.has(msg.action)) refresh()
		})

		// Full state for the newcomer; anything that changed since the last broadcast also goes to
		// the other clients so everyone stays in sync even with polling disabled.
		readState().then(
			(state) => {
				broadcastChanges(state, ws)
				send(state)
			},
			(error) => log.error('Failed to send initial state:', error),
		)
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

	/** Stop and start again (new port / bind address). Disconnects every client. */
	async function restart() {
		log.info('Restarting WebSocket server to apply new configuration.')
		await stop()
		last = { volume: null, muted: null, outputDevice: null }
		return start()
	}

	const sameList = (a = [], b = []) => a.length === b.length && a.every((v, i) => v === b[i])

	/**
	 * Apply the current config after a settings save, touching only what changed:
	 * - port / bind address: restart the listener (unavoidable)
	 * - token / allowed origins: keep listening; disconnect only clients that fail the new rules
	 * - polling: restart the polling timer only
	 * - anything else: nothing
	 * @returns {Promise<{restarted: boolean, disconnected: number, pollingRestarted: boolean}>}
	 */
	async function applyConfig(previous) {
		const next = getConfig()
		const [pw, nw] = [previous.websocket, next.websocket]
		if (!wss || pw.port !== nw.port || pw.host !== nw.host) {
			await restart()
			return { restarted: true, disconnected: 0, pollingRestarted: true }
		}

		let disconnected = 0
		if (pw.token !== nw.token || !sameList(pw.allowedOrigins, nw.allowedOrigins)) {
			for (const client of wss.clients) {
				const handshake = handshakes.get(client)
				if (handshake && admitClient(handshake, nw, 0).ok) continue
				log.info(`Disconnecting ${handshake?.remoteAddress ?? 'client'}: no longer allowed by the access settings`)
				client.close(1008, 'Access settings changed')
				disconnected++
			}
		}

		const [pp, np] = [previous.polling, next.polling]
		const pollingRestarted = pp.enabled !== np.enabled || pp.interval !== np.interval
		if (pollingRestarted) startMonitoring()

		log.info(`Settings applied without restart (disconnected ${disconnected}, polling ${pollingRestarted ? 'updated' : 'unchanged'}).`)
		return { restarted: false, disconnected, pollingRestarted }
	}

	return {
		start,
		stop,
		restart,
		applyConfig,
		refresh,
		readState,
		get clientCount() {
			return wss ? wss.clients.size : 0
		},
	}
}
