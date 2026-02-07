<img src="./assets/banner.png" alt="Audetic" />
Basically superwhisper for Omarchy, Audetic is a voice to text application for Wayland/Hyprland. Press a keybind to toggle recording, get automatic transcription and inject text into the focused application/clipboard...

## Quickstart Video

[![Audetic Quickstart](https://img.youtube.com/vi/8gQLqz_mosI/hqdefault.jpg)](https://youtu.be/8gQLqz_mosI)

- **[View Documentation](./docs/index.md)** - Detailed guides and configuration

## Quick Install (Recommended)

Audetic ships pre-built, signed binaries.

```bash
curl -fsSL https://install.audetic.ai/cli/latest.sh | bash
```

**After installation:**

1. Confirm the service: `audetic` - streams the logs
2. Add a keybind in Hyprland (or your compositor): `bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle`
3. Press the keybind to start/stop recording!

Hold-to-talk keyboard option (Hyprland press/release):

```ini
bind  = SUPER, R, exec, curl -X POST http://127.0.0.1:3737/start
bindr = SUPER, R, exec, curl -X POST http://127.0.0.1:3737/stop
```

## Configuration

Default config at `~/.config/audetic/config.toml`. See [Configuration Guide](./docs/configuration.md) for details.

### Provider CLI

Audetic ships an interactive helper so you can switch transcription providers without editing TOML by hand:

```bash
audetic provider show        # inspect current provider (secrets masked)
audetic provider configure   # interactive wizard (requires a TTY)
audetic provider test        # validate the stored provider
```

### Streaming Overlay (Optional)

Audetic includes an optional low-latency streaming mode with a native overlay UI (live partial text + meter).

```toml
[streaming]
enabled = true
provider = "mistral_realtime"
commit_target = "clipboard"

[overlay]
enabled = true
url = "http://127.0.0.1:3737/stream/events"
```

Set your API key with `MISTRAL_API_KEY` (or `[streaming].api_key`).

Launch options:

```bash
audetic-launch    # starts service if needed, then opens overlay
audetic-overlay   # opens overlay only
audetic-tray      # status tray icon (green/orange/red) with quick actions
```

Install launcher icon for taskbar/app menu:

```bash
./scripts/install-desktop-launcher.sh
```

Optional audio ducking while recording:

```toml
[behavior]
audio_ducking = true
ducking_level_percent = 35
```

## Updates

Audetic includes an auto-updater plus manual controls:

```bash
audetic update
```

## Uninstall

```bash
curl -fsSL https://install.audetic.ai/cli/uninstall.sh | bash
```

Use `--dry-run` to preview, or `--keep-database` to preserve transcription history. See [Installation Guide](./docs/installation.md#uninstalling) for all options.

## License

MIT
