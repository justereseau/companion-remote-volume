// WebSocket server: admission control, command dispatch, state polling and broadcast.
// Independent of Electron so it can be exercised end-to-end in plain Node tests.
import { WebSocketServer, WebSocket } from 'ws'
import {
	ACTIONS,
	ERRORS,
	MAX_PAYLOAD_BYTES,
	RateLimiter,
	TargetNotFoundError,
	admitClient,
	hasCapability,
	normalizeCapabilities,
	parseMessage,
	sameValue,
} from './protocol.js'

const noopLog = { info() {}, warn() {}, error() {} }

/**
 * @param {object} deps
 * @param {() => {websocket: {port, host, token, allowedOrigins}, polling: {enabled, interval}}} deps.getConfig
 * @param {ReturnType<import('./volume.js').createVolumeController>} deps.controller
 * @param {{info, warn, error}} [deps.log]
 * @param {() => RateLimiter} [deps.createRateLimiter]
 * @param {Record<string, object | null>} [deps.actions] action table (see ACTIONS); the device/app actions of
 *   protocol v2 plug in here together with `commands` and `extraState`
 * @param {Record<string, (value: any, message: object) => any>} [deps.commands] handlers for `actions` that
 *   the built-in set does not cover; they receive the parsed `value` and the whole parsed message
 * @param {Record<string, () => any>} [deps.extraState] additional state fields (e.g. list-valued
 *   `outputDevices`, `apps`): read on every refresh, in getState and the initial state, broadcast when
 *   their content changes
 */
export function createVolumeServer({
	getConfig,
	controller,
	log = noopLog,
	createRateLimiter = () => new RateLimiter(),
	actions = ACTIONS,
	commands: extraCommands = {},
	extraState = {},
}) {
	let wss = null
	let pollTimer = null
	let refreshing = null
	let refreshAgain = false
	const STATE_KEYS = ['volume', 'muted', 'outputDevice', ...Object.keys(extraState)]
	const freshLast = () => Object.fromEntries(STATE_KEYS.map((key) => [key, null]))
	let last = freshLast()
	const handshakes = new WeakMap()

	async function readState() {
		const extraKeys = Object.keys(extraState)
		const [volume, muted, outputDevice, ...extras] = await Promise.all([
			controller.getVolume(),
			controller.isMuted(),
			controller.getAudioDevice(),
			...extraKeys.map((key) => extraState[key]()),
		])
		return { volume, muted, outputDevice, ...Object.fromEntries(extraKeys.map((key, i) => [key, extras[i]])) }
	}

	/**
	 * What this machine can do right now, as asked of the controller (its answer depends on the OS and on
	 * what is running, e.g. a Pulse server). A controller without the method supports nothing new.
	 */
	async function readCapabilities() {
		return normalizeCapabilities(await controller.getCapabilities?.())
	}

	const commands = {
		...extraCommands,
		setVolume: (value) => controller.setVolume(value),
		increaseVolume: (value) => controller.increaseVolume(value),
		decreaseVolume: (value) => controller.decreaseVolume(value),
		mute: () => controller.mute(),
		unmute: () => controller.unmute(),
		toggleMute: () => controller.toggleMute(),
		getState: () => readState(),
		getCapabilities: async () => ({ capabilities: await readCapabilities() }),
		isMuted: async () => ({ muted: await controller.isMuted() }),
	}
	const REPLYING_ACTIONS = new Set(['getState', 'getCapabilities', 'isMuted'])
	const MUTATING_ACTIONS = new Set(['setVolume', 'increaseVolume', 'decreaseVolume', 'mute', 'unmute', 'toggleMute'])

	function broadcast(message, exclude = null) {
		if (!wss) return
		const payload = JSON.stringify(message)
		for (const client of wss.clients) {
			if (client !== exclude && client.readyState === WebSocket.OPEN) client.send(payload)
		}
	}

	/**
	 * Broadcast each field whose content differs from what clients were last told, one message per field.
	 * Scalars compare with `===`; lists and objects structurally, against a snapshot so that a controller
	 * mutating the value it handed out cannot hide a change.
	 */
	function broadcastChanges(state, exclude = null) {
		for (const key of STATE_KEYS) {
			if (!sameValue(state[key], last[key])) {
				last[key] = typeof state[key] === 'object' && state[key] !== null ? structuredClone(state[key]) : state[key]
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

			const msg = parseMessage(raw, actions)
			if (!msg.ok) {
				log.warn(`Rejected client message: ${msg.error}`)
				return send(msg.action ? { action: msg.action, response: { error: msg.error } } : { error: msg.error })
			}

			const spec = actions[msg.action]
			let response
			try {
				// Ask before acting: an action the machine cannot do never reaches the controller.
				if (spec?.requires && !hasCapability(await readCapabilities(), spec.requires)) {
					return send({ action: msg.action, response: { error: ERRORS.NOT_SUPPORTED } })
				}
				response = await commands[msg.action](msg.value, msg)
			} catch (error) {
				if (error instanceof TargetNotFoundError) {
					log.warn(`${msg.action}: ${error.message}`)
					return send({ action: msg.action, response: { error: error.message } })
				}
				log.error(`Error executing ${msg.action}:`, error)
				return send({ action: msg.action, response: { error: ERRORS.EXECUTION_FAILED } })
			}
			if (REPLYING_ACTIONS.has(msg.action) || spec?.reply) send(response)
			// Push the resulting state to every client right away, whether or not polling is enabled.
			if (MUTATING_ACTIONS.has(msg.action) || spec?.mutates) refresh()
		})

		// Full state for the newcomer; anything that changed since the last broadcast also goes to
		// the other clients so everyone stays in sync even with polling disabled.
		// The capabilities ride along as one extra key; if probing them fails the client can still ask
		// with getCapabilities, so it never costs the client its state.
		Promise.all([
			readState(),
			readCapabilities().catch((error) => (log.warn('Failed to read capabilities:', error), undefined)),
		]).then(
			([state, capabilities]) => {
				broadcastChanges(state, ws)
				send(capabilities ? { ...state, capabilities } : state)
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
		last = freshLast()
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
