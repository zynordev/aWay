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

> **Linux'ta `media` derlerken:** uzaktan giriş için `enigo` kullanılıyor ve bu, Linux'ta
> `xkbcommon`'a bağlı. `sudo apt install libxkbcommon-dev` gerekir. Windows'ta ek bir şey
> istemez. (CI yalnızca Windows derliyor.)

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
yazıp bağlan.

### Hesap oluşturma

Giriş ekranındaki **"Hesap oluştur"** düğmesi → kullanıcı adı + şifre (iki kez) →
**"Hesabı oluştur"**. Hesap sunucuda açılır ve otomatik giriş yapılır. Kullanıcı adı
zaten varsa sunucu "kullanıcı adı zaten var" der.

Hazır hesaplar: `halil / 123`, `erdem / 456`, `test / test1234`, `test2 / test1234`.

> Sunucu tarafı not: hesap deposu (`accounts.json`) **sunucu açılışında** belleğe okunur.
> Bu yüzden servis çalışırken `away-server adduser` ile hesap eklemek işe yaramaz
> (hatta sunucunun bir sonraki yazımında üzerine yazılır) — hesapları uygulamadaki
> "Hesap oluştur" ile aç, ya da adduser'dan sonra `systemctl restart away-server` yap.

> Yerel geliştirme sunucusu isteyene: `--server ws://127.0.0.1:9000/ws` ile kendi
> `cargo run -p away-server`'ına bağlanabilirsin.

> Geliştirici notu: `--share` / `--connect`'in eski "GUI'siz" halleri yalnızca **çekirdek**
> (media KAPALI) derlemede, M2 data-channel testi içindir. `--features media` ile derlenince
> her zaman tek pencere GUI açılır.

## 4. Notlar / bilinenler

- Renderer varsayılan **glow (OpenGL)**; wgpu ileride (M6) eklenebilir.
- FPS: `--fps 15` (varsayılan). Yakalama+encode tamamen yazılımsal olduğu için CPU maliyeti
  doğrudan fps ile orantılı; güçlü makinede `--fps 30` denenebilir.
- Çözünürlük: varsayılan **otomatik küçültme** — bkz. bölüm 6.
- Encode/track/depacketize/decode hattı openh264 0.6 + webrtc-rs 0.17 API'lerine göre
  yazıldı ve kaynaktan doğrulandı; yine de ilk derlemede küçük bir uyarlama gerekirse
  `encode.rs` / `decode.rs` / `app.rs` / `net.rs` yereldir, hızlı düzeltilir.
- Var: tek pencere GUI (giriş, ana ekran, **Kabul/Ret**, uzak ekran, ekran paylaşımı),
  **uzaktan fare/klavye (M4)**.
- Henüz YOK (sıradaki milestone'lar): ses/pano/dosya (M5), gözetimsiz erişim +
  kalite/çözünürlük ayarları + tepsi ikonu (M6). Şu an tek seferde bir oturum
  (bağlıyken gelen ikinci istek otomatik reddedilir).

## 5. Uzaktan giriş (M4)

Uzak ekranı izlerken üstteki **"Kontrol"** kutusu açıkken (varsayılan) fare ve klavye
karşı makineye gider. Nasıl çalıştığı ve sınırları:

- **Taşıma:** video track'inden ayrı bir WebRTC **data channel** (`"input"`). Kanalı HOST
  açar (offer'ı o ürettiği için data channel offer'dan önce oluşturulmalı), ama akış ters
  yönde: viewer yazar, host `enigo` ile işletim sistemine enjekte eder.
- **Koordinatlar** 0..1 normalize gider; piksele çevirmeyi host kendi çözünürlüğüyle yapar.
  Böylece iki tarafın ekran boyutu/oranı farklı olabilir. Görüntü en-boy oranı korunarak
  çizilir ve kenardaki siyah (letterbox) alana yapılan tıklama karşıya GİTMEZ.
- **Klavye düzeni izleyici tarafında çözülür**: normal yazı `Event::Text` olarak gider, yani
  Türkçe karakterler host'un düzeninden bağımsız doğru düşer. Ctrl/Alt'lı kısayollarda ise
  tuşun kendisi iletilir (Ctrl+C, Ctrl+V…).
- **Basılı kalan tuş koruması:** Ctrl/Shift/Alt durumu her karede farkla türetilir; uzak
  ekrandan çıkınca, "Kontrol"ü kapatınca ya da oturum düşünce hepsi bırakılır (host'ta
  enjektör thread'i kapanırken `enigo` da basılı tuşları serbest bırakır).
- **Bilinen sınırlar:** uzak imleç çizilmiyor (DXGI yakalaması imleci içermez — sen kendi
  imlecinle konumu görürsün, ama karşı taraf fareyi oynatırsa göremezsin). Ctrl+Alt+Del ve
  UAC penceresi gitmez (Windows bunları normal uygulamalara iletmez); host'ta uygulama
  yönetici yetkisiyle çalışmıyorsa yönetici pencerelerine tıklanamaz.

## 6. Performans: gecikme ve CPU (ölçüm + ayarlar)

Video hattı tamamen **yazılımsal**: yakala → BGRA'dan I420'ye çevir → H264 encode → ağ →
decode → I420'den RGB'ye çevir → çiz. Donanım encode (NVENC/QuickSync) henüz yok, bu yüzden
maliyet **doğrudan piksel sayısı × fps** ile orantılıdır. Ayrıca openh264 bu yapılandırmada
**tek çekirdek** kullanır (tek dilim/slice modu thread sayısını 1'e kilitler), yani 8
çekirdekli makinede bile encode tek core'da koşar.

### Ne değişti (bu tur)

- **Renk dönüşümleri elle yazıldı** (`client/src/convert.rs`). openh264'ün kendi
  dönüştürücüleri piksel başına kayan noktalı çalışıyor ve 1080p'de her iki tarafta da
  onlarca ms yiyordu. Yerine tamsayı (fixed-point) BT.601 sürümleri kondu.
- **Küçültme dönüşüme kaynaştırıldı**: ayrı bir ölçekleme geçişi yok, küçültme okurken
  yapılıyor.
- **Değişmeyen kare atlanıyor**: masaüstü hareketsizken encode hiç çalışmıyor (yalnızca
  keyframe zamanı geldiğinde bir kare gider).
- **Kuyruk 8 → 1**: 8 karelik kuyruk 15 fps'te yarım saniyelik sabit gecikme demekti.
- Ekranı paylaşan taraftaki dönen bekleme animasyonu kaldırıldı (her karede yeniden çizim
  istiyordu, yani encode eden makineyi boşuna 60 fps çizime zorluyordu).
- Yan fayda: openh264'ün kendi encoder/decoder çifti arasındaki renk aralığı uyuşmazlığı
  (kısıtlı yaz / tam oku) düzeldi — görüntü artık soluk değil.

### Ayarlar

```powershell
away-client.exe                # otomatik: 1600 pikselden geniş ekranlar küçültülür
away-client.exe --scale 1      # tam çözünürlük (en net, en pahalı)
away-client.exe --scale 2      # yarı çözünürlük (hâlâ ağırsa)
away-client.exe --fps 10       # kare hızını düşür (CPU'yu doğrudan düşürür)
```

Otomatik seçim: 1920 geniş ekran → 960×540, 2560 → 1280×720, 3840 → 1280×720.
**Bulanık geliyorsa** `--scale 1`, **hâlâ ağır/gecikmeliyse** `--scale 2` veya `--fps 10`.

### Ölçüm (tahmin etme, bak)

- **Ekranı paylaşan makinenin konsolunda** 5 saniyede bir şu satır çıkar:

  ```
  ekran 960x540 (1/2) | gönderilen 14.8 fps | atlanan 61 | dönüşüm 3.2 ms | encode 8.1 ms | 1180 kbps
  ```

  `dönüşüm` + `encode` toplamı **1000/fps ms'yi (15 fps'te 66 ms) aşıyorsa** darboğaz
  CPU'dur → `--scale`/`--fps` düşür. Altındaysa gecikme ağdandır.

- **İzleyici tarafında** uzak ekranın üst şeridinde `14 fps · 960×540` yazar. Bu sayı
  host'un "gönderilen" fps'inden belirgin düşükse sorun ağ/bant genişliğidir, encode değil.

Gecikme şikâyeti gelirse: iki sayıyı da (host konsol satırı + izleyici fps) al, hangisinin
düştüğüne bak.
