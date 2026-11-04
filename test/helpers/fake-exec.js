/**
 * Fake callback-style execFile. `handler(file, args)` returns stdout (string) or throws.
 * Every call is recorded in `.calls` as { file, args, options }.
 */
export function fakeExecFile(handler = () => '') {
	const calls = []
	const execFile = (file, args, options, callback) => {
		calls.push({ file, args, options })
		setImmediate(() => {
			let stdout
			try {
				stdout = handler(file, args)
			} catch (error) {
				return callback(error, '', '')
			}
			callback(null, stdout, '')
		})
	}
	execFile.calls = calls
	return execFile
}
