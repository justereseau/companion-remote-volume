# Changelog

## [1.3.0](https://github.com/justereseau/companion-remote-volume/compare/v1.2.0...v1.3.0) (2026-11-05)


### Features

* **core:** all-Rust app with the WebSocket server and a settings page ([f8998a8](https://github.com/justereseau/companion-remote-volume/commit/f8998a8718433e5c0ccf8c4de449767b3e69d1bb))
* **core:** all-Rust app with the WebSocket server and a settings page ([266f783](https://github.com/justereseau/companion-remote-volume/commit/266f783871d1695b4f133707200649efb9960f6d))
* **core:** Rust audio core spike (stdio JSON) ([1ba0478](https://github.com/justereseau/companion-remote-volume/commit/1ba0478042ecb1b9306bc7f3545f027194f1e4b5))
* **core:** Rust audio core spike with stdio JSON contract ([294a618](https://github.com/justereseau/companion-remote-volume/commit/294a618da2238ac39ffb06693d5a01d09f0e66fa)), closes [#8](https://github.com/justereseau/companion-remote-volume/issues/8)


### Bug Fixes

* **core:** WASAPI SetMute takes bool, PropVariantToStringAlloc lives in StructuredStorage ([5c53049](https://github.com/justereseau/companion-remote-volume/commit/5c53049d18c6deeb59b77453725e7dded8ef63b9))

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
