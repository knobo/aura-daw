//! MCP transport security helpers (tauri-free, testable).
//!
//! Threat model: the server listens on 127.0.0.1 only, but ANY local process
//! and — via DNS-rebinding or a malicious web page — the user's browser can
//! reach loopback ports. Defenses (all mandatory, ARCHITECTURE §12.3):
//!
//! 1. Bind 127.0.0.1 only (never 0.0.0.0) — enforced in server.rs.
//! 2. Bearer session token, generated fresh per app launch, constant-time
//!    compared. Delivered to clients out-of-band (shown in the AURA UI /
//!    written 0600 to the app-data dir), NEVER logged in full.
//! 3. Origin validation: browser-originated requests (which carry an Origin
//!    header) are rejected unless the origin is loopback. Absent Origin =
//!    non-browser client = allowed (the token is the gate).

use rand::RngCore;

/// 256-bit random session token, hex-encoded (64 chars).
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// First 8 chars — safe to log / show in the UI status line.
pub fn token_fingerprint(token: &str) -> String {
    token.chars().take(8).collect()
}

/// Constant-time comparison (no early exit on mismatch position).
pub fn token_matches(expected: &str, presented: &str) -> bool {
    let a = expected.as_bytes();
    let b = presented.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Write the full session token to `<data dir>/aura/mcp-token` with 0600
/// permissions so local MCP clients can read it out-of-band (the token never
/// appears in logs or IPC payloads). Returns the file path.
pub fn write_token_file(token: &str) -> Result<std::path::PathBuf, String> {
    let dir = dirs::data_dir()
        .ok_or_else(|| "no platform data dir".to_string())?
        .join("aura");
    write_token_file_in(&dir, token)
}

/// Testable core of [`write_token_file`].
pub fn write_token_file_in(
    dir: &std::path::Path,
    token: &str,
) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join("mcp-token");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        // The file may pre-exist with looser permissions from an old run.
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    file.write_all(token.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Validate an Origin header value (None = header absent).
pub fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else {
        // Non-browser clients (MCP SDKs, curl) send no Origin; the session
        // token remains the authentication gate.
        return true;
    };
    match origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) {
        Some(rest) => {
            let host = rest.split('/').next().unwrap_or("");
            let host = host.rsplit_once(':').map_or(host, |(h, port)| {
                if port.chars().all(|c| c.is_ascii_digit()) { h } else { host }
            });
            matches!(host, "127.0.0.1" | "localhost" | "[::1]")
        }
        // "null" (sandboxed iframe) and anything else: reject.
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_fingerprint_is_short() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_eq!(token_fingerprint(&a).len(), 8);
        assert!(token_matches(&a, &a));
        assert!(!token_matches(&a, &b));
        assert!(!token_matches(&a, &a[..32]));
    }

    #[test]
    fn token_file_is_written_0600_and_overwritten() {
        let dir = std::env::temp_dir().join(format!("aura-mcp-test-{}", std::process::id()));
        let path = write_token_file_in(&dir, "aaaa").unwrap();
        let path2 = write_token_file_in(&dir, "bbbb").unwrap();
        assert_eq!(path, path2);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "bbbb");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn origin_validation_blocks_rebinding() {
        assert!(origin_allowed(None), "non-browser clients pass");
        assert!(origin_allowed(Some("http://127.0.0.1:41717")));
        assert!(origin_allowed(Some("http://localhost:3000")));
        assert!(origin_allowed(Some("https://localhost")));
        assert!(!origin_allowed(Some("http://evil.example.com")));
        assert!(!origin_allowed(Some("http://localhost.evil.com")));
        assert!(!origin_allowed(Some("null")));
        assert!(!origin_allowed(Some("file://")));
    }
}
