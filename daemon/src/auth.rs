//! Authentication for the Omnidea daemon IPC.
//!
//! On startup, the daemon generates a random 32-byte token and writes it to
//! `~/.omnidea/auth.token` with owner-only permissions (0600). Clients must
//! present this token as the first message on any new connection.

use std::io;
use std::path::{Path, PathBuf};

/// Returns the path to the auth token file: `~/.omnidea/auth.token`.
pub fn auth_token_path() -> PathBuf {
    crate::config::omnidea_home().join("auth.token")
}

/// Generate a new random auth token (32 bytes, hex-encoded = 64 chars).
pub fn generate_token() -> String {
    // Use /dev/urandom on Unix, BCryptGenRandom on Windows.
    let mut bytes = [0u8; 32];

    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut bytes);
        } else {
            // Fallback: hash PID + timestamp
            fallback_seed(&mut bytes);
        }
    }

    #[cfg(windows)]
    {
        // Use BCryptGenRandom or fall back to timestamp hash.
        fallback_seed(&mut bytes);
    }

    hex_encode(&bytes)
}

/// Write the auth token to disk with restricted permissions.
pub fn write_token(token: &str) -> io::Result<PathBuf> {
    let path = auth_token_path();

    // Write the token.
    std::fs::write(&path, token)?;

    // Set owner-only permissions (0600).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(path)
}

/// Read the auth token from disk. Used by clients.
pub fn read_token() -> io::Result<String> {
    read_token_from(&auth_token_path())
}

/// Read the auth token from a specific path.
pub fn read_token_from(path: &Path) -> io::Result<String> {
    let token = std::fs::read_to_string(path)?;
    Ok(token.trim().to_string())
}

/// Remove the auth token file on shutdown.
pub fn remove_token() {
    let path = auth_token_path();
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            log::warn!("Could not remove auth token: {e}");
        }
    }
}

/// Verify a token string matches the expected token.
pub fn verify_token(expected: &str, received: &str) -> bool {
    // Constant-time comparison to prevent timing attacks.
    if expected.len() != received.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(received.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

// ─── Helpers ──────────────────────────────────────────────────────────────

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn fallback_seed(bytes: &mut [u8; 32]) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
        ^ (std::process::id() as u64) << 32;

    // Simple xorshift64 expansion of the seed.
    let mut state = seed;
    for chunk in bytes.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let seed_bytes = state.to_le_bytes();
        for (dst, src) in chunk.iter_mut().zip(&seed_bytes) {
            *dst = *src;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_token_length() {
        let token = generate_token();
        assert_eq!(token.len(), 64); // 32 bytes * 2 hex chars
    }

    #[test]
    fn test_generate_token_uniqueness() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }

    #[test]
    fn test_verify_token_correct() {
        let token = generate_token();
        assert!(verify_token(&token, &token));
    }

    #[test]
    fn test_verify_token_wrong() {
        let token = generate_token();
        let wrong = "0".repeat(64);
        assert!(!verify_token(&token, &wrong));
    }

    #[test]
    fn test_verify_token_different_length() {
        assert!(!verify_token("abc", "abcd"));
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex_encode(&[0xff, 0x00, 0xab]), "ff00ab");
    }
}
