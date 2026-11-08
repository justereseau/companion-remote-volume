// Settings page logic (served as /settings.js; inline scripts are blocked by the CSP).
const $ = (id) => document.getElementById(id)

const show = (message, isError = false) => {
	const status = $('status')
	status.textContent = message
	status.classList.toggle('error', isError)
	if (!isError) setTimeout(() => (status.textContent = ''), 3000)
}

const fill = (config) => {
	$('port').value = config.websocket.port
	$('host').value = config.websocket.host
	$('token').value = config.websocket.token
	$('origins').value = config.websocket.allowedOrigins.join('\n')
	$('enablePolling').checked = config.polling.enabled
	$('pollingInterval').value = config.polling.interval
	$('pollingInterval').disabled = !config.polling.enabled
	$('enableApps').checked = config.apps.enabled
	$('enableUpdates').checked = config.updates.enabled
}

const load = async () => {
	try {
		const response = await fetch('/api/config', { cache: 'no-store' })
		if (!response.ok) throw new Error(`HTTP ${response.status}`)
		fill(await response.json())
	} catch (error) {
		show(`Could not load the settings: ${error.message}`, true)
	}
}

const generateToken = () => {
	const bytes = new Uint8Array(24)
	crypto.getRandomValues(bytes)
	return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
}

const save = async () => {
	let port = parseInt($('port').value, 10)
	if (isNaN(port) || port < 1 || port > 65535) port = 2501

	let interval = parseInt($('pollingInterval').value, 10)
	if (isNaN(interval)) interval = 1000
	interval = Math.min(Math.max(interval, 100), 10000)

	// The server re-validates everything; this is only for user feedback.
	const body = {
		websocket: {
			port,
			host: $('host').value,
			token: $('token').value.trim(),
			allowedOrigins: $('origins')
				.value.split('\n')
				.map((line) => line.trim())
				.filter(Boolean),
		},
		polling: { enabled: $('enablePolling').checked, interval },
		apps: { enabled: $('enableApps').checked },
		updates: { enabled: $('enableUpdates').checked },
	}
	try {
		const response = await fetch('/api/config', {
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify(body),
		})
		const result = await response.json().catch(() => ({}))
		if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`)
		fill(result.config)
		show(result.message || 'Configuration saved successfully!')
	} catch (error) {
		show(`Could not save: ${error.message}`, true)
	}
}

const BUSY_STATES = ['checking', 'downloading', 'restarting']

const showUpdate = (update) => {
	const managed = update.managed === true
	$('enableUpdates').disabled = !managed
	$('updateNow').disabled = !managed || BUSY_STATES.includes(update.state)
	if (!managed) {
		$('updateStatus').textContent = `Version ${update.current}. Only an app set up with the installer updates itself.`
		return
	}
	const message = update.message ? ` ${update.message}` : ''
	$('updateStatus').textContent = `Version ${update.current}.${message}`
}

const refreshUpdate = async () => {
	let update
	try {
		const response = await fetch('/api/update', { cache: 'no-store' })
		if (!response.ok) throw new Error(`HTTP ${response.status}`)
		update = await response.json()
	} catch {
		// The app restarts at the end of an update, and the page reconnects on its own.
		$('updateStatus').textContent = 'Waiting for the app to come back...'
		setTimeout(refreshUpdate, 2000)
		return
	}
	showUpdate(update)
	if (BUSY_STATES.includes(update.state)) setTimeout(refreshUpdate, 1000)
}

const updateNow = async () => {
	$('updateNow').disabled = true
	$('updateStatus').textContent = 'Checking for updates...'
	try {
		const response = await fetch('/api/update', { method: 'POST' })
		const result = await response.json().catch(() => ({}))
		if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`)
		setTimeout(refreshUpdate, 500)
	} catch (error) {
		$('updateStatus').textContent = `Could not start the update: ${error.message}`
		$('updateNow').disabled = false
	}
}

$('updateNow').addEventListener('click', updateNow)
$('save').addEventListener('click', save)
$('port').addEventListener('keydown', (event) => {
	if (event.key === 'Enter') save()
})
$('enablePolling').addEventListener('change', (event) => {
	$('pollingInterval').disabled = !event.target.checked
})
$('generateToken').addEventListener('click', () => {
	$('token').value = generateToken()
})

load()
refreshUpdate()
