# aWay istemci — Windows'ta `media` (M3 ekran akışı) derleme & test

Çekirdek istemci (sinyal + data channel) her yerde hafifçe derlenir. **Ekran akışı**
(`media` feature) ekran yakalama (DXGI), H264 encode/decode (openh264) ve egui penceresi
getirdiği için bir C/C++ toolchain ister. Bu adımlar **senin Windows makinende** yapılır
(VDS başsız olduğu için orada test edilemez).

## 1. Gereksinimler (bir kez)

1. **Rust (MSVC)** — https://rustup.rs → varsayılan `stable-x86_64-pc-windows-msvc`.
2. **Visual Studio Build Tools** — "Desktop development with C++" iş yükü
   (cl.exe + link.exe + Windows SDK). openh264-sys2 (cc) ve winit/glow için gerekli.
3. **H264 assembly** için iki seçenekten biri:
   - **nasm** kur (önerilen, daha hızlı encode): `choco install nasm` ardından `nasm`'in
     PATH'te olduğundan emin ol (`nasm --version`). Choco yoksa https://www.nasm.us.
   - **ya da** nasm kurmadan: ortam değişkeni `OPENH264_NO_ASM=1` ver → saf C ile derlenir
     (biraz daha yavaş ama sorunsuz). PowerShell: `$env:OPENH264_NO_ASM=1`

## 2. Derleme

```powershell
# Depo kökünde:
cargo build -p away-client --features media --release
# (nasm kurmadıysan önce:  $env:OPENH264_NO_ASM=1 )
```

Çıktı: `target\release\away-client.exe`

## 3. Çalıştırma — tek pencere (AnyDesk tarzı)

Normal kullanımda **flag yok**: exe'yi çalıştır, açılan pencerede giriş yap. Ana ekranda
kendi kullanıcı adını görürsün ve karşının adını yazıp **Bağlan** dersin; biri sana
bağlanınca **Kabul/Ret** penceresi çıkar. `--user/--pass` verirsen giriş otomatik atlanır
(test kolaylığı), `--connect <ad>` verirsen giriş sonrası otomatik bağlanır.

Sinyal sunucusu **CANLI**: `wss://away.bilgicoderteam.tr/ws` — istemcinin **varsayılanı** budur,
`--server` yazmana gerek yok. Yerel sunucu çalıştırmana gerek yok.

### Hızlı test (canlı sunucuya karşı, iki pencere)

Hazır iki test hesabı var: `test / test1234` ve `test2 / test1234`.

```powershell
# (A) test penceresi — Home'da bekler (gelen bağlantıyı dinler)
target\release\away-client.exe --user test --pass test1234

# (B) test2 penceresi — test'e bağlanır (--connect ile otomatik)
target\release\away-client.exe --user test2 --pass test1234 --connect test
```

Beklenen akış: test2 bağlanınca **test penceresinde "Gelen bağlantı → Kabul et / Reddet"**
çıkar. Kabul et'e basınca test2'nin penceresinde test'in ekranı görünür (tek makinede
"aynalar koridoru" etkisi normaldir — uçtan uca boru hattının çalıştığını kanıtlar).
İki ayrı laptopta: her birinde exe'yi çalıştır, kendi hesabınla giriş yap, birbirinin adını
yazıp bağlan. (Gerçek hesapları sistem sahibi `away-server adduser` ile açar.)

> Yerel geliştirme sunucusu isteyene: `--server ws://127.0.0.1:9000/ws` ile kendi
> `cargo run -p away-server`'ına bağlanabilirsin.

> Geliştirici notu: `--share` / `--connect`'in eski "GUI'siz" halleri yalnızca **çekirdek**
> (media KAPALI) derlemede, M2 data-channel testi içindir. `--features media` ile derlenince
> her zaman tek pencere GUI açılır.

## 4. Notlar / bilinenler

- Renderer varsayılan **glow (OpenGL)**; wgpu ileride (M6) eklenebilir.
- FPS: `--fps 30` (varsayılan). Ağır sahnelerde encode CPU'yu zorlarsa düşür.
- Encode/track/depacketize/decode hattı openh264 0.6 + webrtc-rs 0.17 API'lerine göre
  yazıldı ve kaynaktan doğrulandı; yine de ilk derlemede küçük bir uyarlama gerekirse
  `encode.rs` / `decode.rs` / `app.rs` / `net.rs` yereldir, hızlı düzeltilir.
- Var: tek pencere GUI (giriş, ana ekran, **Kabul/Ret**, uzak ekran, ekran paylaşımı).
- Henüz YOK (sıradaki milestone'lar): uzaktan fare/klavye (M4), ses/pano/dosya (M5),
  gözetimsiz erişim + kalite/çözünürlük ayarları + tepsi ikonu (M6). Şu an tek seferde bir
  oturum (bağlıyken gelen ikinci istek otomatik reddedilir).
```
