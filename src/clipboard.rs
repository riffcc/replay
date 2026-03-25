//! Clipboard support — detect and read clipboard content (text, images, files).

use std::path::PathBuf;
use std::process::Command;

/// What's on the clipboard.
pub enum ClipboardContent {
    /// Plain text.
    Text(String),
    /// Image data saved to a temp file.
    Image(PathBuf),
    /// File path(s) copied to clipboard.
    Files(Vec<PathBuf>),
    /// Nothing useful.
    Empty,
}

/// Read clipboard content, detecting the type automatically.
pub fn read() -> ClipboardContent {
    #[cfg(target_os = "macos")]
    return read_macos();

    #[cfg(target_os = "linux")]
    return read_linux();

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    ClipboardContent::Empty
}

#[cfg(target_os = "macos")]
fn read_macos() -> ClipboardContent {
    // Check for image data first (macOS clipboard can hold images)
    if let Some(path) = read_macos_image() {
        return ClipboardContent::Image(path);
    }

    // Check for file paths (Finder copies)
    if let Some(files) = read_macos_files() {
        return ClipboardContent::Files(files);
    }

    // Fall back to text
    match Command::new("pbpaste").output() {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if text.is_empty() {
                ClipboardContent::Empty
            } else {
                ClipboardContent::Text(text)
            }
        }
        _ => ClipboardContent::Empty,
    }
}

#[cfg(target_os = "macos")]
fn read_macos_image() -> Option<PathBuf> {
    // Check if clipboard has image data using osascript
    let check = Command::new("osascript")
        .args(["-e", "clipboard info for (clipboard info)"])
        .output()
        .ok()?;
    let info = String::from_utf8_lossy(&check.stdout);

    if !info.contains("«class PNGf»") && !info.contains("«class TIFF»") {
        return None;
    }

    // Save clipboard image to temp file as PNG
    let tmp = std::env::temp_dir().join("replay_clipboard.png");
    let script = format!(
        r#"
        set tmpFile to POSIX file "{}"
        try
            set imgData to the clipboard as «class PNGf»
            set fp to open for access tmpFile with write permission
            write imgData to fp
            close access fp
            return "ok"
        on error
            return "fail"
        end try
        "#,
        tmp.display()
    );

    let result = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .ok()?;

    if String::from_utf8_lossy(&result.stdout).trim() == "ok" && tmp.exists() {
        Some(tmp)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn read_macos_files() -> Option<Vec<PathBuf>> {
    // Check for file URLs on the clipboard
    let output = Command::new("osascript")
        .args(["-e", r#"try
            set fileList to the clipboard as «class furl»
            return POSIX path of fileList
        on error
            return ""
        end try"#])
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }

    let files: Vec<PathBuf> = text.lines()
        .map(|l| PathBuf::from(l.trim()))
        .filter(|p| p.exists())
        .collect();

    if files.is_empty() { None } else { Some(files) }
}

#[cfg(target_os = "linux")]
fn read_linux() -> ClipboardContent {
    // Try wl-paste first (Wayland), then xclip (X11)
    let paste_cmd = if Command::new("wl-paste").arg("--version").output().is_ok() {
        "wl-paste"
    } else {
        "xclip"
    };

    // Check for image
    if let Some(path) = read_linux_image(paste_cmd) {
        return ClipboardContent::Image(path);
    }

    // Text fallback
    let output = if paste_cmd == "wl-paste" {
        Command::new("wl-paste").output()
    } else {
        Command::new("xclip").args(["-selection", "clipboard", "-o"]).output()
    };

    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if text.is_empty() {
                ClipboardContent::Empty
            } else {
                ClipboardContent::Text(text)
            }
        }
        _ => ClipboardContent::Empty,
    }
}

#[cfg(target_os = "linux")]
fn read_linux_image(paste_cmd: &str) -> Option<PathBuf> {
    // Check clipboard MIME type for images
    let mime_output = if paste_cmd == "wl-paste" {
        Command::new("wl-paste").args(["--list-types"]).output().ok()?
    } else {
        Command::new("xclip").args(["-selection", "clipboard", "-t", "TARGETS", "-o"]).output().ok()?
    };

    let types = String::from_utf8_lossy(&mime_output.stdout);
    if !types.contains("image/png") && !types.contains("image/jpeg") {
        return None;
    }

    let tmp = std::env::temp_dir().join("replay_clipboard.png");
    let result = if paste_cmd == "wl-paste" {
        Command::new("wl-paste").args(["--type", "image/png"]).output().ok()?
    } else {
        Command::new("xclip").args(["-selection", "clipboard", "-t", "image/png", "-o"]).output().ok()?
    };

    if result.status.success() && !result.stdout.is_empty() {
        std::fs::write(&tmp, &result.stdout).ok()?;
        Some(tmp)
    } else {
        None
    }
}
