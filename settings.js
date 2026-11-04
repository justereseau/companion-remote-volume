// Settings window logic (loaded from index.html; inline scripts are blocked by the CSP).
const $ = (id) => document.getElementById(id)
let currentConfig = null

window.electron.receive('load-config', (config) => {
	currentConfig = config
	$('port').value = config.websocket.port
	$('host').value = config.websocket.host
	$('token').value = config.websocket.token
	$('runOnStartup').checked = config.runOnStartup
	$('enablePolling').checked = config.polling.enabled
	$('pollingInterval').value = config.polling.interval
	$('pollingInterval').disabled = !config.polling.enabled
})

window.electron.receive('config-saved', (message) => {
	$('status').textContent = message
	setTimeout(() => ($('status').textContent = ''), 3000)
})

const generateToken = () => {
	const bytes = new Uint8Array(24)
	crypto.getRandomValues(bytes)
	return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
}

const saveConfig = () => {
	let port = parseInt($('port').value, 10)
	if (isNaN(port) || port < 1 || port > 65535) port = 2501

	const enablePolling = $('enablePolling').checked
	let pollingInterval = parseInt($('pollingInterval').value, 10)
	if (isNaN(pollingInterval)) pollingInterval = 1000
	pollingInterval = Math.min(Math.max(pollingInterval, 100), 10000)

	// The main process re-validates everything; this is only for user feedback.
	window.electron.send('save-config', {
		websocket: {
			port,
			host: $('host').value,
			token: $('token').value.trim(),
			allowedOrigins: currentConfig?.websocket?.allowedOrigins ?? [],
		},
		runOnStartup: $('runOnStartup').checked,
		polling: { enabled: enablePolling, interval: pollingInterval },
	})
}

$('save').addEventListener('click', saveConfig)
$('port').addEventListener('keydown', (event) => {
	if (event.key === 'Enter') saveConfig()
})
$('enablePolling').addEventListener('change', (event) => {
	$('pollingInterval').disabled = !event.target.checked
})
$('generateToken').addEventListener('click', () => {
	$('token').value = generateToken()
})
