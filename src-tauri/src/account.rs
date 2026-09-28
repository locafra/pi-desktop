//! Indirizzo e utente in config.json, password nel portachiavi del sistema
//! (Credential Manager su Windows, Keychain su macOS).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SERVICE: &str = "pi-desktop";

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Account {
    pub url: String,
    pub username: String,
}

impl Account {
    fn key(&self) -> String {
        format!("{}@{}", self.username, self.url)
    }

    fn entry(&self) -> Option<keyring::Entry> {
        keyring::Entry::new(SERVICE, &self.key()).ok()
    }

    pub fn password(&self) -> Option<String> {
        self.entry()?.get_password().ok()
    }

    pub fn store_password(&self, password: &str) -> Result<(), String> {
        self.entry()
            .ok_or("portachiavi non disponibile")?
            .set_password(password)
            .map_err(|e| format!("impossibile salvare la password: {e}"))
    }

    pub fn forget_password(&self) {
        if let Some(e) = self.entry() {
            let _ = e.delete_credential();
        }
    }
}

fn file(dir: &Path) -> PathBuf {
    dir.join("config.json")
}

pub fn load(dir: &Path) -> Option<Account> {
    serde_json::from_slice(&std::fs::read(file(dir)).ok()?).ok()
}

pub fn save(dir: &Path, acc: &Account) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(acc).map_err(|e| e.to_string())?;
    std::fs::write(file(dir), json).map_err(|e| e.to_string())
}

/// "pi.esempio.com" -> "https://pi.esempio.com/"; lo slash finale serve a Url::join.
pub fn normalize_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let with_scheme = if raw.contains("://") { raw.to_string() } else { format!("https://{raw}") };
    let mut u = url::Url::parse(&with_scheme).map_err(|_| "indirizzo non valido".to_string())?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("l'indirizzo deve iniziare con https://".into());
    }
    u.set_query(None);
    u.set_fragment(None);
    if !u.path().ends_with('/') {
        let p = format!("{}/", u.path());
        u.set_path(&p);
    }
    Ok(u.to_string())
}
