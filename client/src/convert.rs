//! Renk dönüşümü ve ölçek düşürme — `media` feature.
//!
//! **Neden elle yazıldı:** openh264 kutusundaki dönüştürücüler (`write_yuv_by_pixel`,
//! `DecodedYUV::write_rgba8`) piksel başına kayan noktalı aritmetik ve sınır denetimli
//! dilim erişimi yapıyor. 1080p'de tek başlarına kare başına onlarca ms yiyorlar — yani
//! ekran akışındaki CPU yükünün ve gecikmenin büyük bölümü encode'a BAŞLAMADAN önce
//! oluşuyordu. Buradaki sürümler tamsayı sabit-nokta aritmetiği kullanır ve satırları
//! `chunks_exact` ile gezer, böylece sınır denetimleri iç döngüden çıkar.
//!
//! **Ayrıca bu iki fonksiyon tutarlı bir çifttir:** encode tarafı BT.601 *sınırlı* aralık
//! (Y 16..235) üretir, decode tarafı da aynı aralığı geri açar. openh264'ün kendi iki
//! fonksiyonu bu konuda uyuşmuyordu (sınırlı yazıp tam aralık okuyordu); görüntü bu
//! yüzden soluk/düşük kontrastlı çıkıyordu, o da düzelmiş oldu.

use egui::Color32;
use openh264::formats::YUVSource;

/// Başlangıç çözünürlüğü: 2560 pikselden geniş ekranlar (4K) yarıya iner.
///
/// Buradan sonrasını ölçüm belirler — `capture` makinenin gerçekten yetiştiği boyuta
/// kendisi iner/çıkar. Bu yalnızca "en fazla bu kadar" başlangıç noktasıdır.
pub fn auto_size(src_w: usize, src_h: usize) -> (usize, usize) {
    const MAX_WIDTH: usize = 2560;
    if src_w <= MAX_WIDTH {
        return (src_w & !1, src_h & !1);
    }
    (src_w / 2 & !1, src_h / 2 & !1)
}

/// Kaynak en-boy oranını koruyarak verilen genişliğe karşılık gelen çift boyut.
pub fn fit_height(src_w: usize, src_h: usize, dst_w: usize) -> usize {
    if src_w == 0 {
        return 0;
    }
    (dst_w * src_h / src_w) & !1
}

/// I420 (YUV 4:2:0) kare tamponu; doğrudan encoder'a verilir.
///
/// Kare başına yeniden ayırmamak için yeniden kullanılır — 1080p'de her kare ~3 MB'lık
/// bir ayırma demekti.
pub struct I420 {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    /// Bir çıktı satırının ortalanmış RGB değerleri (ölçek düşürme ara belleği).
    row: Vec<[u32; 3]>,
    /// Kroma için biriktirilen çift satır (4:2:0 iki satırda bir yazılır).
    crow: Vec<[u32; 3]>,
    /// Her çıktı sütununun kaç kaynak pikselinden beslendiği. Aralık (başlangıç, bitiş)
    /// yerine SAYI tutuluyor: hücreler kaynağı boşluksuz döşediği için kaynak satırı tek
    /// bir sıralı geçişte gezip sayıları tüketmek yeterli — çıktı pikseli başına dilim
    /// kurmak (ve sınır denetimi) ortadan kalkıyor.
    counts: Vec<u32>,
    /// `counts`'un hangi kaynak genişliği/çıktı genişliği için kurulduğu.
    counts_for: (usize, usize),
}

impl I420 {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            y: Vec::new(),
            u: Vec::new(),
            v: Vec::new(),
            row: Vec::new(),
            crow: Vec::new(),
            counts: Vec::new(),
            counts_for: (0, 0),
        }
    }

    /// İki karenin piksel olarak aynı olup olmadığı. Y tek başına yetmez (renk
    /// değişebilir), üç düzleme de bakılır. `Vec<u8>` karşılaştırması memcmp'e iner.
    pub fn same_as(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.y == other.y
            && self.u == other.u
            && self.v == other.v
    }

    fn resize(&mut self, w: usize, h: usize) {
        if self.width == w && self.height == h {
            return;
        }
        self.width = w;
        self.height = h;
        self.y.clear();
        self.y.resize(w * h, 0);
        self.u.clear();
        self.u.resize((w / 2) * (h / 2), 0);
        self.v.clear();
        self.v.resize((w / 2) * (h / 2), 0);
        self.row.clear();
        self.row.resize(w, [0; 3]);
        self.crow.clear();
        self.crow.resize(w / 2, [0; 3]);
    }

    /// Çıktı sütunu -> kaç kaynak pikseli tablosunu (gerekirse) kur.
    ///
    /// Sınırlar `x * src_w / dst_w` ile bölünür: hücreler kaynağı boşluksuz ve üst üste
    /// binmeden döşer, toplamları tam olarak `src_w` eder. Küçültme yaptığımız için her
    /// hücrede en az bir piksel vardır (büyütme çağıran tarafça engelleniyor).
    fn ensure_counts(&mut self, src_w: usize) {
        let dst_w = self.width;
        if self.counts_for == (src_w, dst_w) {
            return;
        }
        self.counts.clear();
        self.counts.reserve(dst_w);
        for x in 0..dst_w {
            let x0 = x * src_w / dst_w;
            let x1 = ((x + 1) * src_w) / dst_w;
            self.counts.push((x1 - x0) as u32);
        }
        self.counts_for = (src_w, dst_w);
    }
}

impl YUVSource for I420 {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }
    fn y(&self) -> &[u8] {
        &self.y
    }
    fn u(&self) -> &[u8] {
        &self.u
    }
    fn v(&self) -> &[u8] {
        &self.v
    }
}

/// BGRA ekran karesini I420'ye çevirir; AYNI geçişte hedef boyuta küçültür.
///
/// `stride` satır adımıdır ve dolgulu olabilir (`stride >= src_w * 4`) — scrap
/// Windows'ta böyle veriyor; bu yüzden önce sıkı bir kopya almaya gerek kalmıyor
/// (eski kod 1080p'de kare başına 8 MB'ı boşuna kopyalıyordu).
///
/// Hedef boyut TAM SAYI BÖLEN OLMAK ZORUNDA DEĞİL: alan ortalaması (box filter) ile
/// keyfi boyuta inilir. 1080p → 720p gibi ara basamaklar bu yüzden mümkün; yalnızca
/// tam bölenlerle 1080p'den sonraki adım 540p olurdu ve aradaki fark çok büyük.
///
/// Çıktı boyutları çifte yuvarlanır: H264 4:2:0 tek boyut kabul etmez.
pub fn bgra_to_i420(
    src: &[u8],
    stride: usize,
    src_w: usize,
    src_h: usize,
    dst: (usize, usize),
    out: &mut I420,
) {
    let (w, h) = (dst.0 & !1, dst.1 & !1);
    if w == 0 || h == 0 || w > src_w || h > src_h {
        return;
    }
    out.resize(w, h);
    out.ensure_counts(src_w);

    // Alanları ayrı ayrı ödünç al: `row`/`crow` ara bellekleri ile `y/u/v` hedefleri
    // aynı anda değiştirilebilsin.
    let I420 { y, u, v, row, crow, counts, .. } = out;
    let uw = w / 2;
    let scaled = w != src_w || h != src_h;

    for oy in 0..h {
        if scaled {
            let y0 = oy * src_h / h;
            let y1 = ((oy + 1) * src_h) / h;
            downsample_row(src, stride, src_w, y0, y1, counts, row);
        } else {
            copy_row(src, stride, oy, row);
        }

        let yr = &mut y[oy * w..oy * w + w];
        for (dst, px) in yr.iter_mut().zip(row.iter()) {
            *dst = rgb_to_y(px[0], px[1], px[2]);
        }

        // Kroma örneği 2×2 çıktı pikselini kapsar: çift satırda biriktir, tekte yaz.
        if oy % 2 == 0 {
            for (acc, px) in crow.iter_mut().zip(row.chunks_exact(2)) {
                acc[0] = px[0][0] + px[1][0];
                acc[1] = px[0][1] + px[1][1];
                acc[2] = px[0][2] + px[1][2];
            }
        } else {
            let ci = (oy / 2) * uw;
            let ur = &mut u[ci..ci + uw];
            let vr = &mut v[ci..ci + uw];
            for (((uo, vo), acc), px) in
                ur.iter_mut().zip(vr.iter_mut()).zip(crow.iter()).zip(row.chunks_exact(2))
            {
                let r = (acc[0] + px[0][0] + px[1][0]) / 4;
                let g = (acc[1] + px[0][1] + px[1][1]) / 4;
                let b = (acc[2] + px[0][2] + px[1][2]) / 4;
                *uo = rgb_to_u(r, g, b);
                *vo = rgb_to_v(r, g, b);
            }
        }
    }
}

/// Ölçekleme yok: kaynak satırını doğrudan BGRA -> RGB olarak al.
fn copy_row(src: &[u8], stride: usize, oy: usize, out: &mut [[u32; 3]]) {
    let row = &src[oy * stride..][..out.len() * 4];
    for (dst, px) in out.iter_mut().zip(row.chunks_exact(4)) {
        *dst = [u32::from(px[2]), u32::from(px[1]), u32::from(px[0])];
    }
}

/// Bir çıktı satırı üret: kaynağın `y0..y1` satırlarını, `counts`'un tanımladığı sütun
/// hücreleri üzerinden ortalar (alan ortalaması / box filter).
///
/// Kaynak satırı TEK SIRALI GEÇİŞTE gezilir: hücreler boşluksuz döşediği için, piksel
/// akışını tüketirken hücre sayacı bittiğinde bir sonraki hücreye geçmek yeterli. Önceki
/// sürüm çıktı pikseli başına `row[x0*4..x1*4]` dilimi kuruyordu; 1920 genişlikte bu, kare
/// başına ~1200 dilim + sınır denetimi demekti ve ölçekli modu 1:1'den pahalı yapıyordu.
fn downsample_row(
    src: &[u8],
    stride: usize,
    src_w: usize,
    y0: usize,
    y1: usize,
    counts: &[u32],
    out: &mut [[u32; 3]],
) {
    for dst in out.iter_mut() {
        *dst = [0; 3];
    }
    for sy in y0..y1 {
        let row = &src[sy * stride..][..src_w * 4];
        let mut px = row.chunks_exact(4);
        for (dst, &cnt) in out.iter_mut().zip(counts) {
            for _ in 0..cnt {
                // `counts` toplamı tam olarak `src_w` olduğundan akış bitmez. Yine de
                // panik yerine sessizce çıkıyoruz: bölme aşağıda yine çalışır, yani en
                // kötü ihtimalle satırın sonu biraz karanlık olur, çöp piksel çıkmaz.
                let Some(p) = px.next() else { break };
                dst[0] += u32::from(p[2]);
                dst[1] += u32::from(p[1]);
                dst[2] += u32::from(p[0]);
            }
        }
    }
    let rows = (y1 - y0) as u32;
    for (dst, &cnt) in out.iter_mut().zip(counts) {
        let d = rows * cnt;
        dst[0] /= d;
        dst[1] /= d;
        dst[2] /= d;
    }
}

// BT.601, sınırlı aralık (studio swing) — H264'ün varsayılan olarak beklediği aralık.
// Katsayılar 8 bit sabit noktaya ölçeklenmiştir, taşma olmaz: Y 16..235, U/V 16..240.

#[inline]
fn rgb_to_y(r: u32, g: u32, b: u32) -> u8 {
    (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8
}

#[inline]
fn rgb_to_u(r: u32, g: u32, b: u32) -> u8 {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8
}

#[inline]
fn rgb_to_v(r: u32, g: u32, b: u32) -> u8 {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8
}

/// Çözülmüş I420 kareyi doğrudan egui piksellerine açar.
///
/// Ara bir RGBA `Vec<u8>` üretilmez: hedef zaten `ColorImage`'ın istediği
/// `Vec<Color32>`. Böylece izleyicide kare başına fazladan bir tam ekran ayırma +
/// tam ekran kopya ortadan kalkar (1080p'de 8 MB'ın iki katı trafik demekti).
///
/// `w` çift olmalıdır (H264 4:2:0 zaten çift üretir); çağıran yuvarlar.
pub fn i420_to_pixels(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    strides: (usize, usize, usize),
    w: usize,
    h: usize,
    out: &mut Vec<Color32>,
) {
    out.clear();
    out.reserve(w * h);
    for r in 0..h {
        let yr = &y[r * strides.0..][..w];
        let cr = r / 2;
        let ur = &u[cr * strides.1..][..w / 2];
        let vr = &v[cr * strides.2..][..w / 2];
        // Bir kroma örneği yatayda iki luma pikselini besler.
        for ((pair, &uu), &vv) in yr.chunks_exact(2).zip(ur).zip(vr) {
            let d = i32::from(uu) - 128;
            let e = i32::from(vv) - 128;
            let (dr, dg, db) = (409 * e + 128, -100 * d - 208 * e + 128, 516 * d + 128);
            for &yy in pair {
                let c = 298 * (i32::from(yy) - 16);
                out.push(Color32::from_rgb(clamp8(c + dr), clamp8(c + dg), clamp8(c + db)));
            }
        }
    }
}

#[inline]
fn clamp8(v: i32) -> u8 {
    (v >> 8).clamp(0, 255) as u8
}
