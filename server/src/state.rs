//! Sunucunun çalışma-anı durumu: online kullanıcılar ve aktif oturumlar.

use away_shared::protocol::ServerMessage;
use rand::Rng;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::users::FileUserStore;

/// Bir bağlantıya mesaj göndermek için kanal ucu.
pub type Tx = mpsc::UnboundedSender<ServerMessage>;

/// İki kullanıcı arasında kurulan (ya da kurulmakta olan) bir oturum.
#[derive(Debug, Clone)]
pub struct Session {
    pub initiator: String,
    pub target: String,
    pub accepted: bool,
}

impl Session {
    /// Verilen kullanıcı bu oturumun tarafı mı; öyleyse KARŞI tarafı döndür.
    pub fn peer_of(&self, user: &str) -> Option<&str> {
        if user == self.initiator {
            Some(&self.target)
        } else if user == self.target {
            Some(&self.initiator)
        } else {
            None
        }
    }
}

pub struct AppState {
    pub cfg: Config,
    pub users: FileUserStore,
    online: Mutex<HashMap<String, Tx>>,
    sessions: Mutex<HashMap<String, Session>>,
}

impl AppState {
    pub fn new(cfg: Config, users: FileUserStore) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            users,
            online: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
        })
    }

    // ── online kullanıcı yönetimi ────────────────────────────────────────────

    /// Kullanıcıyı online yap. Aynı kullanıcı başka yerde bağlıysa eski bağlantı
    /// değiştirilir ve eski `Tx` döndürülür (arayan onu kapatabilir).
    pub fn set_online(&self, user: &str, tx: Tx) -> Option<Tx> {
        self.online.lock().unwrap().insert(user.to_string(), tx)
    }

    /// Kullanıcıyı offline yap — ancak yalnızca kayıtlı `Tx` bu bağlantıya aitse.
    /// (Aynı kullanıcı yeniden bağlandıysa eskisinin kapanışı yenisini düşürmesin.)
    pub fn set_offline_if(&self, user: &str, tx: &Tx) {
        let mut map = self.online.lock().unwrap();
        if let Some(existing) = map.get(user) {
            if existing.same_channel(tx) {
                map.remove(user);
            }
        }
    }

    pub fn is_online(&self, user: &str) -> bool {
        self.online.lock().unwrap().contains_key(user)
    }

    /// Bir kullanıcıya mesaj gönder. Kullanıcı online değilse `false`.
    pub fn send_to(&self, user: &str, msg: ServerMessage) -> bool {
        let tx = self.online.lock().unwrap().get(user).cloned();
        match tx {
            Some(tx) => tx.send(msg).is_ok(),
            None => false,
        }
    }

    // ── oturum yönetimi ──────────────────────────────────────────────────────

    pub fn new_session_id() -> String {
        let mut rng = rand::thread_rng();
        let bytes: [u8; 16] = rng.gen();
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn insert_session(&self, id: String, session: Session) {
        self.sessions.lock().unwrap().insert(id, session);
    }

    pub fn get_session(&self, id: &str) -> Option<Session> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    pub fn accept_session(&self, id: &str) -> Option<Session> {
        let mut map = self.sessions.lock().unwrap();
        let s = map.get_mut(id)?;
        s.accepted = true;
        Some(s.clone())
    }

    pub fn remove_session(&self, id: &str) -> Option<Session> {
        self.sessions.lock().unwrap().remove(id)
    }

    /// Kullanıcının dahil olduğu tüm oturumları kaldır ve karşı taraflarını döndür
    /// (bağlantı koptuğunda karşı tarafa `Hangup` göndermek için).
    pub fn drop_sessions_of(&self, user: &str) -> Vec<(String, String)> {
        let mut map = self.sessions.lock().unwrap();
        let mut affected = Vec::new();
        map.retain(|id, s| {
            if let Some(peer) = s.peer_of(user) {
                affected.push((id.clone(), peer.to_string()));
                false // kaldır
            } else {
                true
            }
        });
        affected
    }
}
