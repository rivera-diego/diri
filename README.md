# Diri Daemon

A lightweight, efficient window management daemon for the [Niri](https://github.com/YaLTeR/niri) window manager, written in Rust.

## Features

- **Unified Fetching**: A smart `fetch` command that brings existing windows to your current workspace or spawns them if they don't exist.
- **Auto-Fill Realignment**: Automatically manages window layout and focus consistency using a throttled event-stream listener.
- **IPC Support**: Communicates via local Unix sockets for lightning-fast command execution.
- **Nirius-compatible Logic**: Implements the robust window matching algorithms (app_id/title regex matching) found in Nirius.
- **Low Footprint**: Ultra-low CPU and memory usage thanks to Rust's asynchronous `tokio` runtime and the `niri-ipc` crate.

## Commands

### `fetch`

The main command for daily use. It automatically includes `--focus` and `--include-current-workspace` flags.

```bash
# Syntax: diri fetch <app_id_regex> <spawn_command...>
$ diri fetch "ghostty" ghostty new
```

### `move-to-current-workspace-or-spawn`

A literal implementation of the Nirius command for granular control via flags.

```bash
$ diri fetch --app-id "dolphin" --focus --include-current-workspace dolphin
```

### `daemon`

Starts the background event listener.

```bash
$ diri daemon
```

## Installation & Autostart

1. Compile and install to your Niri configuration directory:

   ```bash
   cargo install --path . --root ~/.config/niri
   ```

2. Add to your `~/.config/niri/autostart.kdl`:

   ```kdl
   spawn-at-startup "./bin/diri" "daemon"
   ```

3. Configure your keybinds in `~/.config/niri/keybinds.kdl`:
   ```kdl
   Mod+T { spawn "./bin/diri" "fetch" "com.mitchellh.ghostty" "ghostty"; }
   ```

---

_Inspired by [Nirius](https://github.com/tsdh/nirius) and [Piri](https://github.com/vaxerski/Piri)._
