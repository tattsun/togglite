# Togglite

An ultra-lightweight Toggl Track client that lives in the Windows system tray.
Written in Rust against the raw Win32 API: no Electron, no WebView.
The release binary is about 0.6 MB and uses a few MB of private memory.

> Togglite is an unofficial third-party client. Toggl is a trademark of Toggl OÜ.

## Features

- Tray icon shows whether a timer is running (green) or stopped (grey); the tooltip shows the elapsed time.
- Left-click the tray icon or press `Ctrl+Alt+T` for the quick-entry popup:
  - type a description and press Enter to start; click the project chip to pick a project
  - while running, a card with a large timer and a Stop button
  - recent entries restart with a single click (mouse wheel to scroll)
  - Esc or losing focus closes the popup; drag any empty area to move it, and the position is remembered
- Right-click menu: Start/Stop, Open, Reload, Settings, Quit.
- Settings (API token) live on a page inside the popup.

## Design

The popup is custom-drawn with GDI+ (rounded pills, cards and buttons, DWM rounded window corners,
hover states). It follows the Windows app theme (dark/light) automatically. Text uses Segoe UI and
icons use Segoe MDL2 Assets. The only stock control is a borderless Win32 EDIT for the text field,
so the IME keeps working.

## Install

Download `togglite.exe` (or the zip) from the
[latest release](https://github.com/tattsun/togglite/releases/latest) and run it from anywhere.
There is no installer; to start it with Windows, put a shortcut in `shell:startup`.

## Build from source

Requirements: Windows 10/11, [mise](https://mise.jdx.dev/), Visual Studio with the C++ build tools and a Windows SDK.

```powershell
mise install          # installs the pinned Rust toolchain
mise run build        # target/release/togglite.exe
mise run start        # launch
```

On first launch the settings page opens. Paste the API token from the bottom of your Toggl
[Profile settings](https://track.toggl.com/profile) and save.

## Configuration and security

- Settings are stored in `%APPDATA%\togglite\config.json`.
- The API token is encrypted with Windows DPAPI, bound to your user account, so it cannot be decrypted by other users or on other machines.
- The only network endpoint is `https://api.track.toggl.com`. There is no telemetry and no auto-update.
- To render popup menus in the dark theme, Togglite calls the same undocumented uxtheme.dll entry points (ordinals 135/136) that Explorer uses. If they are missing, it silently falls back to the default menu look.

## Development

```powershell
mise run test         # unit tests
mise run dev          # debug build and run
```

A demo mode seeds sample data so the UI can be reviewed without an account. It makes no network calls.

```powershell
$env:TOGGLITE_DEMO = "running"   # or "idle"
$env:TOGGLITE_THEME = "light"    # or "dark"; unset to follow the OS setting
cargo run
```

## Releasing

CI builds and tests every push. Pushing a tag that matches the version in `Cargo.toml`
builds the release binary on GitHub Actions and publishes it with SHA-256 checksums:

```powershell
git tag v0.1.0
git push origin v0.1.0
```

## License

[MIT](LICENSE)
