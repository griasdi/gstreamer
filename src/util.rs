//! Small helpers every step shares.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

/// The repository root: the directory of this crate's manifest as compiled
/// in. The tool is run with `cargo run` from a checkout, and everything it
/// builds lands in that checkout's `target/gstreamer/`.
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Runs `command`, echoing it, and fails when it does.
pub fn run(command: &mut Command) -> Result<()> {
    eprintln!("$ {command:?}");
    let status = command
        .status()
        .with_context(|| format!("failed to run {:?}", command.get_program()))?;
    ensure!(
        status.success(),
        "{:?} failed with {status}",
        command.get_program()
    );
    Ok(())
}

/// Whether `program --version` runs.
pub fn available(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Makes every `git` that `command` starts (the tool's own clones, and the
/// ones meson makes for the FFmpeg and orc subprojects) check files out as
/// upstream committed them: no line-ending conversion, whatever the machine's
/// git configuration says (Git for Windows defaults to `core.autocrlf=true`).
/// The patches apply to those bytes and the source archive carries them.
pub fn git_checkout_env(command: &mut Command) -> &mut Command {
    command.envs([
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "core.autocrlf"),
        ("GIT_CONFIG_VALUE_0", "false"),
        ("GIT_CONFIG_KEY_1", "core.eol"),
        ("GIT_CONFIG_VALUE_1", "lf"),
    ])
}

/// Lower-case hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    out
}

/// The SHA-256 of the file `path`, in hexadecimal.
pub fn sha256_hex(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let n = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex(&hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_pads_every_byte_to_two_digits() {
        assert_eq!(hex(&[0x00, 0x0a, 0xab, 0xff]), "000aabff");
        assert_eq!(hex(&[]), "");
    }
}
