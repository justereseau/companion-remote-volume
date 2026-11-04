// Preload script for the settings window (runs sandboxed, CommonJS).
const { contextBridge, ipcRenderer } = require('electron')

const SEND_CHANNELS = ['save-config']
const RECEIVE_CHANNELS = ['load-config', 'config-saved']

contextBridge.exposeInMainWorld('electron', {
	send: (channel, data) => {
		if (SEND_CHANNELS.includes(channel)) ipcRenderer.send(channel, data)
	},
	receive: (channel, func) => {
		// Never hand the raw IpcRendererEvent (and with it ipcRenderer) to the page.
		if (RECEIVE_CHANNELS.includes(channel)) ipcRenderer.on(channel, (_event, ...args) => func(...args))
	},
})
