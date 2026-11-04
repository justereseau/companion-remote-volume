import { app, BrowserWindow, Tray, Menu, ipcMain, session } from 'electron'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import Store from 'electron-store'
import AutoLaunch from 'electron-auto-launch'
import log from 'electron-log/main'
import { sanitizeConfig } from './lib/config.js'
import { createVolumeServer } from './lib/server.js'
import { controller } from './volumeController.js'

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
	let mainWindow
	let tray
	let isQuitting = false
	const store = new Store()

	let config = sanitizeConfig(store.get('settings'))
	store.set('settings', config)

	const server = createVolumeServer({ getConfig: () => config, controller, log })
	const startServer = (fn) => fn().catch((error) => log.error('Failed to start WebSocket server:', error))

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
		await startServer(server.start)

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
			await startServer(server.restart)
		})
	})

	app.on('will-quit', () => {
		server.stop()
	})

	app.on('window-all-closed', () => {
		// Keep running in the tray; quitting happens through the tray menu.
	})
}
