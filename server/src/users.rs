//! Hesap deposu — kullanıcı adı + argon2 hash'li şifre.
//!
//! v1: dosya-tabanlı (`accounts.json`). Bellek-darı VDS'te ağır `sqlx`/SQLite
//! derlemesinden kaçınmak için bilinçli tercih; 2 kullanıcı için fazlasıyla yeterli.
//! İleride SQLite'a geçiş `UserStore` trait'inin arkasında tek modülde olur.

use anyhow::{anyhow, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Depoda saklanan tek hesap kaydı.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Account {
    username: String,
    /// PHC formatında argon2 hash (salt dahil).
    password_hash: String,
}

/// İleride SQLite vb. ile değiştirilebilir soyutlama.
pub trait UserStore: Send + Sync {
    /// Yeni hesap oluştur. Kullanıcı adı zaten varsa hata.
    fn register(&self, username: &str, password: &str) -> Result<()>;
    /// Kullanıcı adı + şifre doğrula. Doğruysa `Ok(true)`.
    fn verify(&self, username: &str, password: &str) -> Result<bool>;
    /// Hesap var mı?
    fn exists(&self, username: &str) -> bool;
}

/// Dosya-tabanlı `UserStore`. İç durum bellek içinde tutulur, her değişiklikte
/// diske atomik olarak (temp + rename) yazılır.
pub struct FileUserStore {
    path: PathBuf,
    inner: Mutex<HashMap<String, Account>>,
}

impl FileUserStore {
    /// Dosyayı oku (yoksa boş depo). Senkron; başlangıçta bir kez çağrılır.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let map = match std::fs::read(&path) {
            Ok(bytes) => {
                let accounts: Vec<Account> = serde_json::from_slice(&bytes)?;
                accounts.into_iter().map(|a| (a.username.clone(), a)).collect()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, inner: Mutex::new(map) })
    }

    /// Belleği diske atomik yaz. Kilit tutulurken çağrılmalı.
    fn persist_locked(&self, map: &HashMap<String, Account>) -> Result<()> {
        let accounts: Vec<&Account> = map.values().collect();
        let json = serde_json::to_vec_pretty(&accounts)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn hash_password(password: &str) -> Result<String> {
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| anyhow!("argon2 hash: {e}"))?;
        Ok(hash.to_string())
    }
}

impl UserStore for FileUserStore {
    fn register(&self, username: &str, password: &str) -> Result<()> {
        if username.trim().is_empty() || password.is_empty() {
            return Err(anyhow!("boş kullanıcı adı/şifre"));
        }
        let hash = Self::hash_password(password)?;
        // Sync Mutex; argon2 CPU-yoğun olduğu için bu metotlar handler'da
        // `spawn_blocking` içinden çağrılır (async yürütücüyü bloklamaz).
        let mut map = self.inner.lock().unwrap();
        if map.contains_key(username) {
            return Err(anyhow!("kullanıcı adı zaten var"));
        }
        map.insert(
            username.to_string(),
            Account { username: username.to_string(), password_hash: hash },
        );
        self.persist_locked(&map)?;
        Ok(())
    }

    fn verify(&self, username: &str, password: &str) -> Result<bool> {
        let map = self.inner.lock().unwrap();
        let Some(acc) = map.get(username) else {
            return Ok(false);
        };
        let parsed = PasswordHash::new(&acc.password_hash)
            .map_err(|e| anyhow!("hash parse: {e}"))?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok())
    }

    fn exists(&self, username: &str) -> bool {
        self.inner.lock().unwrap().contains_key(username)
    }
}
