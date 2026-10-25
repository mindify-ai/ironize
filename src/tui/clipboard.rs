//! Copying to and pasting from the system clipboard.
//!
//! Copies go two ways: an OSC 52 escape sequence, which the terminal turns
//! into a clipboard write (and which works over SSH), and the platform's
//! clipboard tool when one is installed locally.

use std::io::Write;
use std::process::{Command, Stdio};

/// Copies `text` to the clipboard. Never blocks on the clipboard tool.
pub fn copy(text: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(osc52(text).as_bytes());
    let _ = out.flush();
    if std::env::var_os("SSH_CONNECTION").is_some() {
        return; // a local tool would copy on the remote machine
    }
    let text = text.to_string();
    std::thread::spawn(move || {
        for (program, args) in copy_commands() {
            if pipe_into(program, args, &text) {
                return;
            }
        }
    });
}

/// Reads text from the system clipboard.
pub fn paste() -> Result<String, String> {
    for (program, args) in paste_commands() {
        let Ok(output) = Command::new(program)
            .args(*args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            continue;
        };
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
    }
    Err("no clipboard tool found".to_string())
}

/// The OSC 52 sequence that sets the clipboard to `text`.
pub fn osc52(text: &str) -> String {
    let sequence = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    if std::env::var_os("TMUX").is_some() {
        // tmux takes the plain sequence with `set-clipboard on`, and forwards
        // the wrapped one to the outer terminal with `allow-passthrough on`.
        let wrapped = format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"));
        format!("{sequence}{wrapped}")
    } else {
        sequence
    }
}

fn pipe_into(program: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().is_ok_and(|s| s.success())
}

type Commands = &'static [(&'static str, &'static [&'static str])];

fn copy_commands() -> Commands {
    if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(windows) {
        &[("clip.exe", &[])]
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])]
    } else {
        &[
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    }
}

fn paste_commands() -> Commands {
    if cfg!(target_os = "macos") {
        &[("pbpaste", &[])]
    } else if cfg!(windows) {
        &[(
            "powershell.exe",
            &["-NoProfile", "-Command", "Get-Clipboard -Raw"],
        )]
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[
            ("wl-paste", &["--no-newline"]),
            ("xclip", &["-selection", "clipboard", "-o"]),
        ]
    } else {
        &[
            ("xclip", &["-selection", "clipboard", "-o"]),
            ("xsel", &["--clipboard", "--output"]),
        ]
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("héllo\n".as_bytes()), "aMOpbGxvCg==");
    }

    #[test]
    fn builds_osc52() {
        if std::env::var_os("TMUX").is_none() {
            assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        }
    }
}
