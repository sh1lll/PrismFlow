//! Companion API for remote devices (docs/security/remote-access.md).
//! Default off. When on, it listens with TLS on one user-chosen address,
//! pairs devices with one-time codes, and gives them read-only access (plus
//! a few actions if the user allows them per device). It never exposes keys,
//! tokens or the gateway.

pub mod backend;
pub mod server;
pub mod tls;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const DEVICE_TOKEN_PREFIX: &str = "pfd_";
pub const PAIRING_TTL: Duration = Duration::from_secs(300);
pub const PAIRING_MAX_ATTEMPTS: u32 = 5;

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).expect("the OS random number generator must be available");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn digest_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|x| format!("{x:02x}")).collect()
}

/// Constant-time comparison of two equal-length hex digests.
fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// SHA-256 of the device token; the token itself is only on the device.
    pub token_digest: String,
    /// May run verification, restart the gateway and switch models.
    pub allow_actions: bool,
    pub created_at: u64,
    pub last_seen: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionSettings {
    pub address: String,
    pub port: u16,
    /// Turn back on at app start; otherwise remote access ends with the app.
    pub keep_on: bool,
}

impl Default for CompanionSettings {
    fn default() -> Self {
        Self { address: String::new(), port: 47822, keep_on: false }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    devices: Vec<Device>,
    #[serde(default)]
    settings: CompanionSettings,
}

struct Pairing {
    code_digest: String,
    expires: Instant,
    attempts: u32,
}

/// Device registry + the single outstanding pairing code.
pub struct Registry {
    file: PathBuf,
    data: Mutex<Persisted>,
    pairing: Mutex<Option<Pairing>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PairError {
    NoCode,
    Expired,
    Wrong,
    TooManyAttempts,
}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PairError::NoCode => "no pairing is in progress; start one on the desktop",
            PairError::Expired => "the pairing code expired; start a new one on the desktop",
            PairError::Wrong => "wrong pairing code",
            PairError::TooManyAttempts => "too many wrong codes; the code was cancelled. Start a new one on the desktop",
        })
    }
}

impl Registry {
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let file = dir.join("companion.json");
        let data = match std::fs::read_to_string(&file) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", file.display()))?,
            Err(_) => Persisted::default(),
        };
        Ok(Self { file, data: Mutex::new(data), pairing: Mutex::new(None) })
    }

    fn save(&self, data: &Persisted) -> Result<(), String> {
        let body = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
        crate::config_engine::atomic_write(&self.file, &body).map_err(|e| e.to_string())
    }

    pub fn settings(&self) -> CompanionSettings {
        self.data.lock().unwrap_or_else(|e| e.into_inner()).settings.clone()
    }

    pub fn set_settings(&self, s: CompanionSettings) -> Result<(), String> {
        let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
        d.settings = s;
        self.save(&d)
    }

    pub fn devices(&self) -> Vec<Device> {
        self.data.lock().unwrap_or_else(|e| e.into_inner()).devices.clone()
    }

    /// Start pairing: a fresh 128-bit code replaces any outstanding one.
    pub fn start_pairing(&self) -> String {
        let code = random_hex(16);
        *self.pairing.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Pairing { code_digest: digest_hex(&code), expires: Instant::now() + PAIRING_TTL, attempts: 0 });
        code
    }

    pub fn cancel_pairing(&self) {
        *self.pairing.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Exchange a pairing code for a device token (returned once).
    pub fn pair(&self, code: &str, device_name: &str, now: u64) -> Result<(Device, String), PairError> {
        let mut p = self.pairing.lock().unwrap_or_else(|e| e.into_inner());
        let Some(pending) = p.as_mut() else { return Err(PairError::NoCode) };
        if Instant::now() > pending.expires {
            *p = None;
            return Err(PairError::Expired);
        }
        if !ct_eq(&digest_hex(code.trim()), &pending.code_digest) {
            pending.attempts += 1;
            if pending.attempts >= PAIRING_MAX_ATTEMPTS {
                *p = None;
                return Err(PairError::TooManyAttempts);
            }
            return Err(PairError::Wrong);
        }
        *p = None; // single use
        drop(p);
        let token = format!("{DEVICE_TOKEN_PREFIX}{}", random_hex(32));
        let name: String = device_name.trim().chars().filter(|c| !c.is_control()).take(40).collect();
        let device = Device {
            id: random_hex(8),
            name: if name.is_empty() { "Device".into() } else { name },
            token_digest: digest_hex(&token),
            allow_actions: false,
            created_at: now,
            last_seen: Some(now),
        };
        let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
        d.devices.push(device.clone());
        let _ = self.save(&d);
        Ok((device, token))
    }

    /// The device a token belongs to, updating last-seen.
    pub fn authenticate(&self, token: &str, now: u64) -> Option<Device> {
        if !token.starts_with(DEVICE_TOKEN_PREFIX) {
            return None;
        }
        let dg = digest_hex(token);
        let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let dev = d.devices.iter_mut().find(|x| ct_eq(&x.token_digest, &dg))?;
        dev.last_seen = Some(now);
        Some(dev.clone())
    }

    pub fn revoke(&self, id: &str) -> Result<(), String> {
        let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let before = d.devices.len();
        d.devices.retain(|x| x.id != id);
        if d.devices.len() == before {
            return Err("Unknown device.".into());
        }
        self.save(&d)
    }

    pub fn set_allow_actions(&self, id: &str, allow: bool) -> Result<(), String> {
        let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let dev = d.devices.iter_mut().find(|x| x.id == id).ok_or("Unknown device.")?;
        dev.allow_actions = allow;
        self.save(&d)
    }
}

/// The pairing link encoded in the QR code.
pub fn pairing_uri(base_url: &str, fingerprint: &str, code: &str) -> String {
    format!("prismflow://pair?u={base_url}&fp={fingerprint}&c={code}")
}

pub fn qr_svg(text: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(code.render::<qrcode::render::svg::Color>().min_dimensions(220, 220).build())
}

/// The primary LAN address: the local side of a UDP "connection" to a
/// public address. No packet is sent.
pub fn primary_address() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr().ok().map(|a| a.ip().to_string()).filter(|ip| ip != "0.0.0.0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_is_single_use_and_tokens_authenticate() {
        let dir = tempfile::tempdir().unwrap();
        let r = Registry::open(dir.path()).unwrap();
        assert_eq!(r.pair("x", "phone", 1).unwrap_err(), PairError::NoCode);
        let code = r.start_pairing();
        assert_eq!(code.len(), 32, "128-bit code");
        let (dev, token) = r.pair(&code, "  Alex's\nphone ", 1).unwrap();
        assert!(token.starts_with("pfd_") && token.len() == 68);
        assert_eq!(dev.name, "Alex'sphone");
        assert!(!dev.allow_actions, "read-only by default");
        assert_eq!(r.pair(&code, "again", 2).unwrap_err(), PairError::NoCode, "single use");
        assert_eq!(r.authenticate(&token, 5).unwrap().last_seen, Some(5));
        assert!(r.authenticate("pfd_wrong", 5).is_none());
        assert!(r.authenticate("pft_other", 5).is_none());
        let raw = std::fs::read_to_string(dir.path().join("companion.json")).unwrap();
        assert!(!raw.contains(&token) && !raw.contains(&code), "only digests at rest");
        r.revoke(&dev.id).unwrap();
        assert!(r.authenticate(&token, 6).is_none(), "revoked");
    }

    #[test]
    fn wrong_codes_burn_the_code() {
        let dir = tempfile::tempdir().unwrap();
        let r = Registry::open(dir.path()).unwrap();
        let code = r.start_pairing();
        for _ in 0..PAIRING_MAX_ATTEMPTS - 1 {
            assert_eq!(r.pair("nope", "x", 1).unwrap_err(), PairError::Wrong);
        }
        assert_eq!(r.pair("nope", "x", 1).unwrap_err(), PairError::TooManyAttempts);
        assert_eq!(r.pair(&code, "x", 1).unwrap_err(), PairError::NoCode, "even the right code is now dead");
    }

    #[test]
    fn devices_and_settings_persist() {
        let dir = tempfile::tempdir().unwrap();
        let r = Registry::open(dir.path()).unwrap();
        let code = r.start_pairing();
        let (dev, token) = r.pair(&code, "tablet", 1).unwrap();
        r.set_allow_actions(&dev.id, true).unwrap();
        r.set_settings(CompanionSettings { address: "10.0.0.2".into(), port: 1234, keep_on: true }).unwrap();
        let r2 = Registry::open(dir.path()).unwrap();
        assert!(r2.authenticate(&token, 2).unwrap().allow_actions);
        assert_eq!(r2.settings().port, 1234);
    }

    #[test]
    fn qr_and_uri() {
        let uri = pairing_uri("https://10.0.0.2:47822", "ab12", "c0de");
        assert_eq!(uri, "prismflow://pair?u=https://10.0.0.2:47822&fp=ab12&c=c0de");
        assert!(qr_svg(&uri).unwrap().contains("<svg"));
    }
}
