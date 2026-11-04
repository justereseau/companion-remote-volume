# Changelog

## [1.2.0](https://github.com/justereseau/companion-remote-volume/compare/v1.1.0...v1.2.0) (2026-11-04)


### Features

* add the isMuted action and report Windows helper errors with their HRESULT ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
* optional access token, browser-origin blocking and a "this computer only" bind address for the WebSocket server ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
* push the new state to all connected clients right after every command, even with polling disabled ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
* release with release-please ([3ab8ff5](https://github.com/justereseau/companion-remote-volume/commit/3ab8ff59350f706c60b56a607cab97679ae6fb9a))
* release with release-please ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))


### Bug Fixes

* limit message size, command rate and number of clients on the WebSocket server ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
* saving settings restarts the WebSocket server only when the port or bind address changes ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
* setting or decreasing the volume to 0 had no effect on Windows ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
* settings window clipped its title and footer ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
* stop rapid getState requests from blocking the app by caching the output device name ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
* update Electron 33 (end of life) to 45 and ws to 8.22 to resolve published security advisories ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
* validate settings and IPC messages, sandbox the settings window and enable Electron fuses ([75c99e4](https://github.com/justereseau/companion-remote-volume/commit/75c99e46df88d546f62c69396478d7db6f51aabb))
