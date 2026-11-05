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
