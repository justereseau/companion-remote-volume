import { app, BrowserWindow, Tray, Menu, ipcMain, session } from 'electron'
import { WebSocketServer } from 'ws'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import Store from 'electron-store'
import AutoLaunch from 'electron-auto-launch'
import log from 'electron-log/main'
import { sanitizeConfig } from './lib/config.js'
import { parseMessage, admitClient, RateLimiter, MAX_PAYLOAD_BYTES } from './lib/protocol.js'
import {
	setVolume,
	getVolume,
	mute,
	unmute,
	toggleMute,
	isMuted,
	increaseVolume,
	decreaseVolume,
	getAudioDevice,
} from './volumeController.js'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const INDEX_HTML = path.join(__dirname, 'index.html')
const INDEX_URL = pathToFileURL(INDEX_HTML).href

log.initialize()
log.transports.file.resolvePathFn = () => path.join(app.getPath('userData'), 'logs', 'main.log')
log.info('App is starting...')

// Enforce the Chromium sandbox for every renderer.
app.enableSandbox()

const gotLock = app.requestSingleInstanceLock()

if (!gotLock) {
	log.info('Another instance of the app is already running. Exiting.')
	app.quit()
} else {
	let wss
	let mainWindow
	let tray
	let isQuitting = false
	const store = new Store()

	let config = sanitizeConfig(store.get('settings'))
	store.set('settings', config)

	const setAutoLaunch = async (enabled) => {
		log.info(`Auto-start setting changed. Attempting to ${enabled ? 'enable' : 'disable'} auto-launch.`)
		try {
			if (process.platform === 'linux') {
				const launcher = new AutoLaunch({ name: 'RemoteVolume', path: app.getPath('exe') })
				if (enabled) await launcher.enable()
				else await launcher.disable()
			} else {
				app.setLoginItemSettings({ openAtLogin: enabled, openAsHidden: true })
			}
			log.info(`Auto-launch ${enabled ? 'enabled' : 'disabled'} on ${process.platform}.`)
		} catch (error) {
			log.error('Failed to change auto-launch setting:', error)
		}
	}

	function showWindow() {
		if (!mainWindow || mainWindow.isDestroyed()) createWindow()
		mainWindow.show()
		mainWindow.focus()
	}

	function createWindow() {
		mainWindow = new BrowserWindow({
			width: 400,
			height: 640,
			resizable: false,
			webPreferences: {
				contextIsolation: true,
				nodeIntegration: false,
				sandbox: true,
				webSecurity: true,
				preload: path.join(__dirname, 'preload.cjs'),
			},
			show: true,
			icon: path.join(__dirname, 'icon.ico'),
		})

		// The settings window must never navigate away or open new windows.
		mainWindow.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
		mainWindow.webContents.on('will-navigate', (event) => event.preventDefault())

		mainWindow.webContents.on('did-finish-load', () => {
			mainWindow.webContents.send('load-config', config)
		})

		mainWindow.loadFile(INDEX_HTML)
		mainWindow.setMenu(null)

		mainWindow.on('close', (event) => {
			if (!isQuitting) {
				event.preventDefault()
				mainWindow.hide()
			}
		})
	}

	app.on('second-instance', showWindow)
	app.on('activate', showWindow)
	app.on('before-quit', () => {
		isQuitting = true
	})

	app.whenReady().then(async () => {
		// The settings UI needs no browser permissions (camera, notifications, ...).
		session.defaultSession.setPermissionRequestHandler((_wc, _permission, callback) => callback(false))
		session.defaultSession.setPermissionCheckHandler(() => false)

		createWindow()
		if (process.platform === 'darwin') app.dock.hide()

		tray = new Tray(path.join(__dirname, 'menuicon.png'))
		tray.setToolTip('Remote Volume')
		tray.setContextMenu(
			Menu.buildFromTemplate([
				{ label: 'Show App', click: showWindow },
				{
					label: 'Quit',
					click: () => {
						isQuitting = true
						app.quit()
					},
				},
			]),
		)
		tray.on('click', showWindow)

		await setAutoLaunch(config.runOnStartup)
		startWebSocketServer()

		ipcMain.on('save-config', async (event, newConfig) => {
			// Only accept configuration from our own settings page.
			if (event.senderFrame?.url !== INDEX_URL) {
				log.warn('Rejected save-config from unexpected sender:', event.senderFrame?.url)
				return
			}

			const previous = config
			config = sanitizeConfig(newConfig, previous)
			store.set('settings', config)
			event.reply('load-config', config)
			event.reply('config-saved', 'Configuration saved successfully!')

			if (previous.runOnStartup !== config.runOnStartup) await setAutoLaunch(config.runOnStartup)
			restartWebSocketServer()
		})
	})

	let lastVolume = null
	let lastMuteState = null
	let lastAudioDevice = null
	let pollingInterval = null
	let pollInFlight = false

	async function readState() {
		const [volume, muted, outputDevice] = await Promise.all([getVolume(), isMuted(), getAudioDevice()])
		return { volume, muted, outputDevice }
	}

	function startMonitoring() {
		if (pollingInterval) clearInterval(pollingInterval)
		pollingInterval = null
		if (!config.polling.enabled) return

		log.info(`Starting volume/mute state monitoring every ${config.polling.interval} ms.`)
		pollingInterval = setInterval(async () => {
			// Skip a tick instead of piling up child processes when the OS is slow.
			if (pollInFlight) return
			pollInFlight = true
			try {
				const { volume, muted, outputDevice } = await readState()
				if (volume !== lastVolume) {
					lastVolume = volume
					broadcastState({ volume })
				}
				if (muted !== lastMuteState) {
					lastMuteState = muted
					broadcastState({ muted })
				}
				if (outputDevice !== lastAudioDevice) {
					lastAudioDevice = outputDevice
					broadcastState({ outputDevice })
				}
			} catch (error) {
				log.error('Error monitoring volume/mute state:', error)
			} finally {
				pollInFlight = false
			}
		}, config.polling.interval)
	}

	function broadcastState(state) {
		if (!wss) return
		const payload = JSON.stringify(state)
		for (const client of wss.clients) {
			if (client.readyState === client.OPEN) client.send(payload)
		}
	}

	const commands = {
		setVolume: (value) => setVolume(value),
		increaseVolume: (value) => increaseVolume(value),
		decreaseVolume: (value) => decreaseVolume(value),
		mute: () => mute(),
		unmute: () => unmute(),
		toggleMute: () => toggleMute(),
		getState: () => readState(),
		isMuted: async () => ({ muted: await isMuted() }),
	}

	function restartWebSocketServer() {
		if (!wss) return startWebSocketServer()
		log.info('Closing existing WebSocket server to apply new configuration.')
		const old = wss
		wss = null
		for (const client of old.clients) client.terminate()
		old.close(() => {
			log.info('WebSocket server closed. Restarting...')
			startWebSocketServer()
		})
	}

	function startWebSocketServer() {
		const { port, host } = config.websocket

		const server = new WebSocketServer({
			port,
			host,
			maxPayload: MAX_PAYLOAD_BYTES,
			perMessageDeflate: false,
			verifyClient: ({ req }, done) => {
				const verdict = admitClient(req, config.websocket, server.clients.size)
				if (verdict.ok) return done(true)
				log.warn(`Rejected WebSocket client ${req.socket.remoteAddress}: ${verdict.reason}`)
				done(false, verdict.code, verdict.reason)
			},
		})
		wss = server

		server.on('listening', () => {
			log.info(`WebSocket server listening on ws://${host}:${port}${config.websocket.token ? ' (token required)' : ''}`)
		})

		server.on('error', (error) => {
			log.error('WebSocket server error:', error)
		})

		server.on('connection', async (ws, req) => {
			log.info(`Client connected from ${req.socket.remoteAddress}`)
			const limiter = new RateLimiter()

			ws.on('error', (error) => log.warn('WebSocket client error:', error.message))

			ws.on('message', async (raw, isBinary) => {
				if (isBinary) return
				if (!limiter.tryRemove()) {
					ws.send(JSON.stringify({ error: 'Rate limit exceeded' }))
					return
				}

				const msg = parseMessage(raw)
				if (!msg.ok) {
					log.warn(`Rejected client message: ${msg.error}`)
					ws.send(JSON.stringify(msg.action ? { action: msg.action, response: { error: msg.error } } : { error: msg.error }))
					return
				}

				try {
					const response = await commands[msg.action](msg.value)
					if (msg.action === 'getState' || msg.action === 'isMuted') ws.send(JSON.stringify(response))
				} catch (error) {
					log.error(`Error executing ${msg.action}:`, error)
					ws.send(JSON.stringify({ action: msg.action, response: { error: 'Failed to execute action' } }))
				}
			})

			try {
				ws.send(JSON.stringify(await readState()))
			} catch (error) {
				log.error('Failed to send initial state:', error)
			}
		})

		startMonitoring()
	}

	app.on('window-all-closed', () => {
		// Keep running in the tray; quitting happens through the tray menu.
	})
}
