// electron-builder afterSign hook: notarize macOS builds when Apple credentials are available.
// Forks and local builds without credentials skip notarization instead of failing.
export default async function notarizing(context) {
	const { electronPlatformName, appOutDir } = context

	if (electronPlatformName !== 'darwin') {
		console.log('Skipping notarization (not macOS)')
		return
	}

	const { APPLE_ID, APPLE_ID_PASSWORD, APPLE_TEAM_ID } = process.env
	if (!APPLE_ID || !APPLE_ID_PASSWORD || !APPLE_TEAM_ID) {
		console.log('Skipping notarization (APPLE_ID / APPLE_ID_PASSWORD / APPLE_TEAM_ID not set)')
		return
	}

	const { notarize } = await import('@electron/notarize')
	const appName = context.packager.appInfo.productFilename
	console.log('Starting notarization!')

	return notarize({
		appPath: `${appOutDir}/${appName}.app`,
		appleId: APPLE_ID,
		appleIdPassword: APPLE_ID_PASSWORD,
		teamId: APPLE_TEAM_ID,
	})
}
