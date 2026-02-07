use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use which::which;

#[derive(Clone)]
pub struct TextIoService {
    inner: Arc<TextIoInner>,
}

struct TextIoInner {
    clipboard: Mutex<Option<Clipboard>>,
    preserve_previous: bool,
    pending_restore: Mutex<Option<String>>,
    injection_method: InjectionMethod,
}

impl TextIoService {
    pub fn new(preferred_method: Option<&str>, preserve_previous: bool) -> Result<Self> {
        let clipboard = match Clipboard::new() {
            Ok(cb) => Some(cb),
            Err(err) => {
                warn!(
                    "System clipboard backend unavailable ({}); falling back to CLI-only mode",
                    err
                );
                None
            }
        };
        let injection_method = InjectionMethod::detect(preferred_method);

        Ok(Self {
            inner: Arc::new(TextIoInner {
                clipboard: Mutex::new(clipboard),
                preserve_previous,
                pending_restore: Mutex::new(None),
                injection_method,
            }),
        })
    }

    pub fn injection_method(&self) -> InjectionMethod {
        self.inner.injection_method
    }

    pub async fn copy_to_clipboard(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        info!("Copying {} chars to clipboard", text.len());
        debug!("Text to copy: {}", text);

        let preserve_previous = self.inner.preserve_previous;
        let mut previous: Option<String> = None;
        let mut used_native = false;

        {
            let mut clipboard_guard = self.inner.clipboard.lock().await;
            if let Some(clipboard) = clipboard_guard.as_mut() {
                if preserve_previous {
                    previous = clipboard.get_text().ok();
                }

                match clipboard.set_text(text) {
                    Ok(_) => {
                        used_native = true;
                    }
                    Err(err) => {
                        warn!(
                            "Primary clipboard backend failed ({}), disabling until restart",
                            err
                        );
                        *clipboard_guard = None;
                    }
                }
            } else {
                debug!("Native clipboard backend unavailable; using system clipboard tools");
            }
        }

        if !used_native {
            self.copy_with_system_backends(text).await?;
        }

        // Also set primary selection (Shift+Insert paste on Wayland).
        if which("wl-copy").is_ok() {
            if let Ok(mut child) = Command::new("wl-copy")
                .args(["--type", "text/plain", "--primary"])
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
            }
        }

        if let Some(prev) = previous {
            debug!("Previous clipboard content preserved: {} chars", prev.len());
            let mut pending = self.inner.pending_restore.lock().await;
            *pending = Some(prev);
        }

        Ok(())
    }

    pub async fn inject_text(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        info!("Injecting text: {} chars", text.len());
        debug!("Text to inject: {}", text);

        match self.inner.injection_method {
            InjectionMethod::Wtype => {
                self.try_with_clipboard_fallback(text, Self::inject_with_wtype)
                    .await
            }
            InjectionMethod::Ydotool => {
                self.try_with_clipboard_fallback(text, Self::inject_with_ydotool)
                    .await
            }
            InjectionMethod::Clipboard => self.simulate_paste().await,
        }
    }

    pub async fn paste_from_clipboard(&self) -> Result<()> {
        self.simulate_paste().await
    }

    pub async fn send_enter_key(&self) -> Result<()> {
        info!("Sending Enter key");

        if which("ydotool").is_ok() {
            if let Ok(output) = Command::new("ydotool")
                .args(["key", "28:1", "28:0"])
                .output()
            {
                if output.status.success() {
                    debug!("Sent Enter with ydotool");
                    return Ok(());
                }
                debug!(
                    "ydotool Enter failed: status={:?} stderr={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }

        if which("wtype").is_ok() {
            if let Ok(output) = Command::new("wtype").args(["-k", "Return"]).output() {
                if output.status.success() {
                    debug!("Sent Enter with wtype");
                    return Ok(());
                }
                debug!(
                    "wtype Return failed: status={:?} stderr={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }

        if which("xdotool").is_ok() {
            if let Ok(output) = Command::new("xdotool").args(["key", "Return"]).output() {
                if output.status.success() {
                    debug!("Sent Enter with xdotool");
                    return Ok(());
                }
            }
        }

        Err(anyhow!("Unable to send Enter key with available backends"))
    }

    async fn try_with_clipboard_fallback<F>(&self, text: &str, inject_fn: F) -> Result<()>
    where
        F: Fn(&str) -> Result<()>,
    {
        if let Err(err) = inject_fn(text) {
            warn!(
                "Direct text injection failed with {} – falling back to clipboard paste",
                err
            );
            self.copy_to_clipboard(text).await?;
            self.simulate_paste().await
        } else {
            Ok(())
        }
    }

    async fn copy_with_system_backends(&self, text: &str) -> Result<()> {
        for backend in CLIPBOARD_BACKENDS {
            if which(backend.copy_cmd).is_err() {
                continue;
            }

            let mut cmd = Command::new(backend.copy_cmd);
            cmd.args(backend.copy_args);

            if backend.use_stdin {
                cmd.stdin(Stdio::piped());
            }

            if let Ok(mut child) = cmd.spawn() {
                if backend.use_stdin {
                    if let Some(stdin) = child.stdin.as_mut() {
                        if stdin.write_all(text.as_bytes()).is_err() {
                            continue;
                        }
                    }
                }

                if let Ok(status) = child.wait() {
                    if status.success() {
                        debug!("Text copied to clipboard with {}", backend.name);
                        return Ok(());
                    }
                }
            }
        }

        Err(anyhow!(
            "No clipboard tool (wl-copy/xclip/xsel) available for fallback"
        ))
    }

    async fn restore_preserved_clipboard(&self) -> Result<()> {
        if !self.inner.preserve_previous {
            return Ok(());
        }

        let previous = {
            let mut pending = self.inner.pending_restore.lock().await;
            pending.take()
        };

        let Some(previous) = previous else {
            return Ok(());
        };

        let mut restored = false;
        {
            let mut clipboard_guard = self.inner.clipboard.lock().await;
            if let Some(clipboard) = clipboard_guard.as_mut() {
                match clipboard.set_text(previous.clone()) {
                    Ok(_) => {
                        restored = true;
                    }
                    Err(err) => {
                        warn!(
                            "Primary clipboard backend failed while restoring ({}), disabling until restart",
                            err
                        );
                        *clipboard_guard = None;
                    }
                }
            }
        }

        if !restored {
            self.copy_with_system_backends(&previous).await?;
        }

        if which("wl-copy").is_ok() {
            if let Ok(mut child) = Command::new("wl-copy")
                .args(["--type", "text/plain", "--primary"])
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(previous.as_bytes());
                }
                let _ = child.wait();
            }
        }

        debug!("Restored previous clipboard content after successful paste");
        Ok(())
    }

    fn inject_with_wtype(text: &str) -> Result<()> {
        let output = Command::new("wtype")
            .arg(text)
            .output()
            .context("Failed to execute wtype")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("wtype failed: {}", stderr));
        }

        Ok(())
    }

    fn inject_with_ydotool(text: &str) -> Result<()> {
        let output = Command::new("ydotool")
            .arg("type")
            .arg(text)
            .output()
            .context("Failed to execute ydotool")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("ydotool failed: {}", stderr);
            return Err(anyhow!(
                "ydotool failed: {}. Make sure ydotoold is running",
                stderr
            ));
        }

        Ok(())
    }

    async fn simulate_paste(&self) -> Result<()> {
        info!("Simulating paste from clipboard");
        let mut pasted_successfully = false;

        if which("ydotool").is_ok() {
            debug!("Trying ydotool paste (Ctrl+Shift+V)");
            if let Ok(output) = Command::new("ydotool")
                .args(["key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"])
                .output()
            {
                if output.status.success() {
                    debug!("Successfully pasted with ydotool (Ctrl+Shift+V)");
                    pasted_successfully = true;
                }
                debug!(
                    "ydotool Ctrl+Shift+V failed: status={:?} stderr={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }

        if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
            if desktop == "KDE" && which("ydotool").is_ok() {
                debug!("Trying ydotool paste (Shift+Insert)");
                if let Ok(output) = Command::new("ydotool")
                    .args(["key", "42:1", "110:1", "110:0", "42:0"])
                    .output()
                {
                    if output.status.success() {
                        debug!("Successfully pasted with ydotool (Shift+Insert)");
                        pasted_successfully = true;
                    }
                    debug!(
                        "ydotool Shift+Insert failed: status={:?} stderr={}",
                        output.status.code(),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }

        if which("wtype").is_ok() {
            if let Ok(output) = Command::new("wtype")
                .args([
                    "-M", "ctrl", "-M", "shift", "-P", "v", "-m", "shift", "-m", "ctrl",
                ])
                .output()
            {
                if output.status.success() {
                    debug!("Successfully pasted with wtype (Ctrl+Shift+V)");
                    pasted_successfully = true;
                } else {
                    debug!("wtype paste failed, trying other methods");
                }
            }
        }

        if which("xdotool").is_ok() {
            if let Ok(output) = Command::new("xdotool").args(["key", "ctrl+v"]).output() {
                if output.status.success() {
                    debug!("Successfully pasted with xdotool");
                    pasted_successfully = true;
                }
            }
        }

        if pasted_successfully {
            if let Err(err) = self.restore_preserved_clipboard().await {
                warn!("Failed to restore previous clipboard content: {}", err);
            }
            return Ok(());
        }

        warn!("All paste methods failed - text remains in clipboard for manual paste");
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum InjectionMethod {
    Wtype,
    Ydotool,
    Clipboard,
}

impl InjectionMethod {
    fn detect(preferred: Option<&str>) -> Self {
        if let Some(choice) = preferred {
            match choice {
                "ydotool" if which("ydotool").is_ok() => {
                    info!("Using ydotool for text injection (per config)");
                    return InjectionMethod::Ydotool;
                }
                "wtype" if which("wtype").is_ok() => {
                    info!("Using wtype for text injection (per config)");
                    return InjectionMethod::Wtype;
                }
                "clipboard" => {
                    info!("Using clipboard-based injection (per config)");
                    return InjectionMethod::Clipboard;
                }
                other => {
                    warn!(
                        "Unknown or unavailable input_method '{}', falling back to auto-detect",
                        other
                    );
                }
            }
        }

        if which("ydotool").is_ok() {
            info!("Using ydotool for text injection (auto-detected)");
            return InjectionMethod::Ydotool;
        }

        if std::env::var("WAYLAND_DISPLAY").is_ok() && which("wl-copy").is_ok() {
            info!("Using clipboard-based injection (Wayland detected)");
            return InjectionMethod::Clipboard;
        }

        if which("wtype").is_ok() {
            info!("Using wtype for text injection (auto-detected)");
            return InjectionMethod::Wtype;
        }

        info!("Falling back to clipboard-based injection");
        InjectionMethod::Clipboard
    }
}

struct ClipboardBackend {
    name: &'static str,
    copy_cmd: &'static str,
    copy_args: &'static [&'static str],
    use_stdin: bool,
}

const CLIPBOARD_BACKENDS: &[ClipboardBackend] = &[
    ClipboardBackend {
        name: "wl-copy",
        copy_cmd: "wl-copy",
        copy_args: &["--type", "text/plain"],
        use_stdin: true,
    },
    ClipboardBackend {
        name: "xclip",
        copy_cmd: "xclip",
        copy_args: &["-selection", "clipboard"],
        use_stdin: true,
    },
    ClipboardBackend {
        name: "xsel",
        copy_cmd: "xsel",
        copy_args: &["--clipboard", "--input"],
        use_stdin: true,
    },
];

/// Copy text to clipboard using system clipboard tools (synchronous version).
///
/// Uses wl-copy (Wayland), xclip, or xsel (X11) for persistent clipboard
/// storage that survives after the process exits.
///
/// This is a standalone function for use in synchronous contexts (e.g., CLI commands).
pub fn copy_to_clipboard_sync(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    for backend in CLIPBOARD_BACKENDS {
        if which(backend.copy_cmd).is_err() {
            continue;
        }

        let mut child = match Command::new(backend.copy_cmd)
            .args(backend.copy_args)
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => continue,
        };

        if let Some(stdin) = child.stdin.as_mut() {
            if stdin.write_all(text.as_bytes()).is_err() {
                continue;
            }
        }

        if let Ok(status) = child.wait() {
            if status.success() {
                return Ok(());
            }
        }
    }

    Err(anyhow!(
        "No clipboard tool available. Please install wl-copy (Wayland), xclip, or xsel (X11)."
    ))
}
