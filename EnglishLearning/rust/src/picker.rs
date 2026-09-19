//! Native OS file picker used by the web UI.
//!
//! Browsers deliberately hide the real local path of a file chosen with
//! `<input type="file">`, so the page cannot hand the server a usable path.
//! Instead the page asks the server (which runs on the same machine in the
//! default `127.0.0.1` setup) to open the OS file dialog and return the
//! absolute path it produced.
//!
//! One dialog tool is used per platform:
//!   * Linux   — `zenity`, falling back to `kdialog`
//!   * macOS   — `osascript` (`choose file`)
//!   * Windows — PowerShell + `System.Windows.Forms.OpenFileDialog`
//!
//! A cancelled dialog is not an error: it returns `Ok(None)`.

use std::process::Command;

/// Open the OS file dialog. `kind` is `"media"` (default) or `"subtitle"` and
/// only selects the dialog's file filter.
pub fn pick_file(kind: &str) -> Result<Option<String>, String> {
    #[cfg(target_os = "linux")]
    {
        pick_linux(kind)
    }
    #[cfg(target_os = "macos")]
    {
        pick_macos(kind)
    }
    #[cfg(target_os = "windows")]
    {
        pick_windows(kind)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = kind;
        Err("the native file picker is not supported on this platform".to_string())
    }
}

fn dialog_title(kind: &str) -> &'static str {
    if kind == "subtitle" {
        "Select a subtitle file (.srt / .vtt)"
    } else {
        "Select a media file"
    }
}

/// True when `program` exists in `PATH`.
fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// Interpret a dialog process result: stdout path on success, `None` on cancel.
fn dialog_output(out: std::process::Output) -> Result<Option<String>, String> {
    if out.status.success() {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Ok(if path.is_empty() { None } else { Some(path) });
    }
    // zenity/kdialog/osascript all exit 1 when the user cancels.
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
        "the file dialog failed to open".to_string()
    } else {
        err
    })
}

#[cfg(target_os = "linux")]
fn pick_linux(kind: &str) -> Result<Option<String>, String> {
    let title = dialog_title(kind);
    if which("zenity") {
        let mut cmd = Command::new("zenity");
        cmd.arg("--file-selection").arg(format!("--title={title}"));
        if kind == "subtitle" {
            cmd.arg("--file-filter=Subtitles | *.srt *.vtt").arg("--file-filter=All files | *");
        } else {
            cmd.arg("--file-filter=Media | *.mp4 *.mkv *.mov *.webm *.m4a *.mp3 *.wav *.ogg *.aac")
                .arg("--file-filter=All files | *");
        }
        let out = cmd
            .output()
            .map_err(|e| format!("failed to launch zenity: {e}"))?;
        return dialog_output(out);
    }
    if which("kdialog") {
        let start = std::env::current_dir().unwrap_or_default();
        let filter = if kind == "subtitle" {
            "*.srt *.vtt"
        } else {
            "*.mp4 *.mkv *.mov *.webm *.m4a *.mp3 *.wav *.ogg *.aac"
        };
        let out = Command::new("kdialog")
            .arg("--title")
            .arg(title)
            .arg("--getopenfilename")
            .arg(start)
            .arg(filter)
            .output()
            .map_err(|e| format!("failed to launch kdialog: {e}"))?;
        return dialog_output(out);
    }
    Err("no native file dialog found: install `zenity` (GNOME) or `kdialog` (KDE)".to_string())
}

#[cfg(target_os = "macos")]
fn pick_macos(kind: &str) -> Result<Option<String>, String> {
    let prompt = dialog_title(kind);
    let types = if kind == "subtitle" {
        "\"srt\", \"vtt\""
    } else {
        "\"mp4\", \"mkv\", \"mov\", \"webm\", \"m4a\", \"mp3\", \"wav\", \"ogg\", \"aac\""
    };
    let script = format!("POSIX path of (choose file with prompt \"{prompt}\" of type {{{types}}})");
    let out = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("failed to launch osascript: {e}"))?;
    dialog_output(out)
}

#[cfg(target_os = "windows")]
fn pick_windows(kind: &str) -> Result<Option<String>, String> {
    let title = dialog_title(kind);
    let filter = if kind == "subtitle" {
        "Subtitle files (*.srt;*.vtt)|*.srt;*.vtt|All files (*.*)|*.*"
    } else {
        "Media files (*.mp4;*.mkv;*.mov;*.webm;*.m4a;*.mp3;*.wav;*.ogg;*.aac)|*.mp4;*.mkv;*.mov;*.webm;*.m4a;*.mp3;*.wav;*.ogg;*.aac|All files (*.*)|*.*"
    };
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.OpenFileDialog; \
         $d.Title = '{title}'; $d.Filter = '{filter}'; \
         if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) \
         {{ [Console]::Out.Write($d.FileName) }}"
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .map_err(|e| format!("failed to launch powershell: {e}"))?;
    dialog_output(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_depend_on_kind() {
        assert!(dialog_title("subtitle").contains("subtitle"));
        assert!(dialog_title("media").contains("media"));
        assert!(dialog_title("anything-else").contains("media"));
    }
}
