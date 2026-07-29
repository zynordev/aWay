# aWay — VDS Kurulum Rehberi (Sinyal Sunucusu + TURN)

Bu adımlar VDS'te (Debian 12, IP `38.210.77.34`) sinyal sunucusunu ve coturn relay'i
canlıya alır. İstemci (Windows/Linux) M2'den itibaren ayrı geliştirilecek.

> Ön koşul: Bir alt alan adı, örn. `away.bilgicoderteam.tr`, VDS IP'sine **A kaydı** ile
> yönlendirilmeli. (Senin domainin var — sadece bu subdomain'i eklemen yeterli.)

## 1) Gizli anahtar üret
```bash
openssl rand -hex 32     # çıktıyı hem turnserver.conf hem env dosyasına koy
```

## 2) coturn (TURN/STUN relay)
```bash
# deploy/turnserver.conf içinde: static-auth-secret, realm, server-name'i düzenle
docker compose -f deploy/docker-compose.yml up -d
docker logs away_coturn --tail 20     # "IPv4. ... relay" satırlarını gör
```
Firewall'da şu portları aç (VDS sağlayıcı panelinden de): `3478/udp`, `3478/tcp`,
`5349/tcp` (TLS), ve relay aralığı `49160-49200/udp`.

## 3) Sinyal sunucusunu kur
```bash
# Sürüm binary'sini derle (bu repoda):
~/.cargo/bin/cargo build --release --manifest-path Cargo.toml -p away-server

sudo useradd --system --no-create-home away 2>/dev/null || true
sudo mkdir -p /var/lib/away /etc/away
sudo chown away:away /var/lib/away
sudo cp target/release/away-server /usr/local/bin/away-server
sudo cp deploy/env.example /etc/away/env      # DÜZENLE: secret, domain
sudo cp deploy/away-server.service /etc/systemd/system/

# Hesapları aç (açık kayıt kapalı olduğu için CLI ile):
sudo -u away AWAY_ACCOUNTS=/var/lib/away/accounts.json away-server adduser SEN sifren
sudo -u away AWAY_ACCOUNTS=/var/lib/away/accounts.json away-server adduser ARKADAS sifresi

sudo systemctl daemon-reload
sudo systemctl enable --now away-server
systemctl status away-server --no-pager
curl -s http://127.0.0.1:9000/healthz     # -> ok
```

## 4) nginx + TLS
```bash
sudo cp deploy/nginx-away.conf /etc/nginx/sites-available/away.conf
# Dosyada away.bilgicoderteam.tr -> gerçek subdomain ile değiştir
sudo ln -s /etc/nginx/sites-available/away.conf /etc/nginx/sites-enabled/away.conf
sudo nginx -t
sudo certbot --nginx -d away.bilgicoderteam.tr   # sertifika + otomatik yönlendirme
sudo systemctl reload nginx
```

Doğrulama:
```bash
curl -s https://away.bilgicoderteam.tr/healthz   # -> ok
# WebSocket uç noktası: wss://away.bilgicoderteam.tr/ws
```

## Notlar / Güvenlik
- Sunucu belleği systemd'de `MemoryMax=128M` ile sınırlı — VDS'teki Mastodon/r-place'i korur.
- Açık kayıt varsayılan KAPALI. Yeni hesap için `adduser` kullan.
- TURN kimlik bilgileri kısa ömürlü (HMAC ile üretilir); coturn'a kalıcı kullanıcı yazılmaz.
- Sunucu WebRTC medyasını görmez; sadece kullanıcı adı doğrular ve SDP/ICE iletir.
