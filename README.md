# aWay — Kullanıcı adı tabanlı uzaktan masaüstü (AnyDesk benzeri)

Sadece iki kişi (sen + arkadaşın) için, **kullanıcı adı ile** (ID yok) bağlanan, sıfırdan
Rust ile yazılan uzaktan masaüstü aracı. VDS, AnyDesk'in sunucularının işini görür:
kullanıcı adı doğrulama + buluşturma (signaling) + relay (TURN). Medya doğrudan (P2P) akar,
olmazsa VDS üzerinden relay'e düşer.

## Yapı (cargo workspace)
- `shared/` — sunucu ve istemcinin ortak konuştuğu **signaling protokolü** (`protocol.rs`).
  Android APK ileride aynı JSON şeklini kullanacak; alan adları stabil tutulur.
- `server/` — VDS'te çalışan **sinyal sunucusu** (axum WS). Kayıt/giriş (argon2 + dosya
  deposu), presence, bağlantı yönlendirme, SDP/ICE aktarımı, kabul/ret, kısa ömürlü TURN
  kimlik bilgisi üretimi. WebRTC medyasını **görmez**.
- `client/` — Windows/Linux native istemci. Şu an iskelet; ağır medya bağımlılıkları
  (webrtc, ekran yakalama, giriş, ses, UI) **M2**'den itibaren eklenir.
- `deploy/` — coturn (Docker), nginx reverse proxy, systemd birimi, kurulum rehberi.

## Durum
- **M0 — Temel & protokol:** ✅ tamam.
- **M1 — Sinyal sunucusu:** ✅ kod tamam, testler geçiyor (uçtan uca signaling akışı dahil).
  Canlıya alma `deploy/SETUP.md` — alan adı + `docker compose` + nginx/certbot ile.
- **M2 — İstemci taşıma çekirdeği:** ✅ tamam ve doğrulandı. İki istemci loopback'te
  webrtc-rs ile P2P bağlantı + data channel kuruyor (`connected`, çift yönlü mesaj).
- **M3 — Ekran akışı + tek pencere GUI:** 🟡 kod tamam, **Windows testi bekliyor**. `media`
  cargo feature'ı arkasında **AnyDesk tarzı tek pencereli uygulama** (eframe/egui, glow):
  giriş → ana ekran (kendi kullanıcı adın + bağlan kutusu, gelen bağlantıları dinler) →
  **Kabul/Ret** penceresi → uzak ekran / ekran paylaşımı. Boru hattı: scrap/DXGI yakalama →
  H264 encode (openh264) → WebRTC video track → viewer'da decode + pencerede çizim. Tüm harici
  API'ler kaynaktan doğrulandı. VDS'te derlenmez (C codec + bellek); Windows'ta
  `--features media` ile derlenip test edilir → **[`client/BUILD-WINDOWS.md`](client/BUILD-WINDOWS.md)**.
- M4 giriş, M5 ses/pano/dosya, M6 UX cila (gözetimsiz erişim, kalite ayarları, tepsi),
  M7 Android APK — planlı.

### Mimari (media/GUI)
Tek `away-client.exe`. Ana thread'de **eframe GUI** (`app.rs`), arka planda ayrı tokio
runtime'ında **ağ motoru** (`net.rs`) sinyal+WebRTC'yi yürütür; ikisi `Arc<Mutex<UiState>>`
+ `UiCommand` kanalı ile konuşur. Bağlantıyı **viewer** başlatır ama WebRTC **offer**'ını
ekrana sahip **host** üretir (viewer answer'lar). Modüller: `client/src/{app,net,capture,
encode,decode,video,frame}.rs`. Çekirdek (media kapalı) derlemede eski M2 data-channel testi
(`--connect` arayan / boş bekleyen) aynen çalışır — CI/loopback doğrulaması için.

### M2 testini çalıştırma (loopback, tek makine)
```bash
cargo build -p away-server -p away-client
# İki uç: bekleyen + arayan (bkz. client/src/main.rs başlığı)
away-client --user ahmet --pass p2                    # bekleyen
away-client --user murat --pass p1 --connect ahmet    # arayan
# Başarı: her iki tarafta "RECV_OK" + "bağlantı durumu: connected"
```

## Geliştirme
```bash
cargo build                 # tüm workspace
cargo test                  # birim + entegrasyon testleri
cargo run -p away-server    # sunucuyu yerelde çalıştır (AWAY_* env ile)
```

## Mimari notu
Signaling omurgası WebRTC (`webrtc-rs`) — istemci tarafında. Sunucu ise `SignalPayload`'ı
opak kabul edip yalnızca hedef kullanıcıya iletir; böylece protokol kırılmadan evrilebilir
(APK dahil). Kurulum ve güvenlik ayrıntıları: [`deploy/SETUP.md`](deploy/SETUP.md).
