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
//! A cancelled dialog is not an error: it returns no paths.

use std::process::Command;

/// Open the OS file dialog for one file. `kind` is `"media"` (default) or
/// `"subtitle"` and only selects the dialog's file filter.
pub fn pick_file(kind: &str) -> Result<Option<String>, String> {
    // A cancelled single-selection dialog is an empty list, so this collapses
    // back to `None` and the two functions cannot drift apart.
    Ok(pick(kind, false)?.pop())
}

/// Open the OS file dialog for several files at once (`--multiple` /
/// `Multiselect`). Returns an empty list when the user cancels.
///
/// The queue page needs this: dubbing a folder of clips is one gesture, not a
/// dialog per file.
pub fn pick_files(kind: &str) -> Result<Vec<String>, String> {
    pick(kind, true)
}

fn pick(kind: &str, multi: bool) -> Result<Vec<String>, String> {
    #[cfg(target_os = "linux")]
    {
        pick_linux(kind, multi)
    }
    #[cfg(target_os = "macos")]
    {
        pick_macos(kind, multi)
    }
    #[cfg(target_os = "windows")]
    {
        pick_windows(kind, multi)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (kind, multi);
        Err("the native file picker is not supported on this platform".to_string())
    }
}

/// Show `path` in the OS file manager.
///
/// The server runs on the same machine as the browser in the default loopback
/// setup, so a folder it opens is the one the user is looking at. The spawned
/// program is detached: its exit code says nothing useful (Windows `explorer`
/// reports failure even on success), so only the spawn is checked.
pub fn open_dir(path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .arg(path)
            .spawn()
            .map_err(|e| format!("failed to launch explorer: {e}"))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("failed to launch open: {e}"))?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("failed to launch xdg-open: {e}"))?;
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = path;
        Err("opening a directory is not supported on this platform".to_string())
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

/// Split a dialog's stdout into paths: one per line, blank lines dropped.
fn paths_of(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

/// Interpret a dialog process result as the list of chosen paths: one entry per
/// line, empty on cancel.
///
/// Every tool below is asked for a newline separator, so a single selection and
/// a multi-selection arrive through the same code path.
fn dialog_output(out: std::process::Output) -> Result<Vec<String>, String> {
    if out.status.success() {
        return Ok(paths_of(&out.stdout));
    }
    // zenity/kdialog/osascript all exit 1 when the user cancels.
    if out.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
        "the file dialog failed to open".to_string()
    } else {
        err
    })
}

#[cfg(target_os = "linux")]
fn pick_linux(kind: &str, multi: bool) -> Result<Vec<String>, String> {
    let title = dialog_title(kind);
    if which("zenity") {
        let mut cmd = Command::new("zenity");
        cmd.arg("--file-selection").arg(format!("--title={title}"));
        if multi {
            // zenity joins selections with `--separator`; a newline keeps it
            // identical to the single-file output shape.
            cmd.arg("--multiple").arg("--separator=\n");
        }
        if kind == "subtitle" {
            cmd.arg("--file-filter=Subtitles | *.srt *.vtt")
                .arg("--file-filter=All files | *");
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
            "*.srt *.vtt".to_string()
        } else {
            "*.mp4 *.mkv *.mov *.webm *.m4a *.mp3 *.wav *.ogg *.aac".to_string()
        };
        let mut cmd = Command::new("kdialog");
        cmd.arg("--title")
            .arg(title)
            .arg("--getopenfilename")
            .arg(start);
        if multi {
            // `--multiple` already implies one path per line for each
            // `--getopenfilename` call.
            cmd.arg("--multiple");
        }
        let out = cmd
            .arg(filter)
            .output()
            .map_err(|e| format!("failed to launch kdialog: {e}"))?;
        return dialog_output(out);
    }
    Err("no native file dialog found: install `zenity` (GNOME) or `kdialog` (KDE)".to_string())
}

#[cfg(target_os = "macos")]
fn pick_macos(kind: &str, multi: bool) -> Result<Vec<String>, String> {
    let prompt = dialog_title(kind);
    let types = if kind == "subtitle" {
        "\"srt\", \"vtt\""
    } else {
        "\"mp4\", \"mkv\", \"mov\", \"webm\", \"m4a\", \"mp3\", \"wav\", \"ogg\", \"aac\""
    };
    // `with multiple selections allowed` yields a list, so the paths are joined
    // by a linefeed delimiter to match the other platforms' output.
    let choose = if multi {
        "choose file with prompt \"{prompt}\" of type {types} with multiple selections allowed"
    } else {
        "choose file with prompt \"{prompt}\" of type {types}"
    };
    let script = format!(
        "set theFiles to ({choose})\n\
         set thePaths to {{}}\n\
         repeat with f in theFiles\n\
         set end of thePaths to POSIX path of f\n\
         end repeat\n\
         set AppleScript's text item delimiters to linefeed\n\
         return thePaths as text"
    );
    let out = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("failed to launch osascript: {e}"))?;
    dialog_output(out)
}

#[cfg(target_os = "windows")]
fn pick_windows(kind: &str, multi: bool) -> Result<Vec<String>, String> {
    let title = dialog_title(kind);
    let filter = if kind == "subtitle" {
        "Subtitle files (*.srt;*.vtt)|*.srt;*.vtt|All files (*.*)|*.*"
    } else {
        "Media files (*.mp4;*.mkv;*.mov;*.webm;*.m4a;*.mp3;*.wav;*.ogg;*.aac)|*.mp4;*.mkv;*.mov;*.webm;*.m4a;*.mp3;*.wav;*.ogg;*.aac|All files (*.*)|*.*"
    };
    // `[char]10` is the linefeed joined between `FileNames`, matching the shape
    // the other platforms produce (`FileName` is the single-selection twin).
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.OpenFileDialog; \
         $d.Title = '{title}'; $d.Filter = '{filter}'; $d.Multiselect = ${multi}; \
         if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) \
         {{ [Console]::Out.Write(($d.FileNames -join [char]10)) }}"
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

    /// A single selection and a multi-selection only differ in how many lines
    /// the dialog prints, so one parser has to handle both.
    #[test]
    fn a_dialog_yields_one_path_per_line() {
        assert_eq!(paths_of(b"/media/a.mp4\n"), vec!["/media/a.mp4"]);
        assert_eq!(
            paths_of(b"/media/a.mp4\n/media/b.mkv\n"),
            vec!["/media/a.mp4", "/media/b.mkv"]
        );
        // Cancelling prints nothing; a trailing newline must not invent a path.
        assert!(paths_of(b"").is_empty());
        assert!(paths_of(b"\n").is_empty());
    }
}
