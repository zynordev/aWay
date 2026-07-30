//! Tek pencereli eframe arayüzü (AnyDesk tarzı) — `media` feature.
//!
//! Ana thread'de çalışır; ağ/webrtc işini arka plandaki `net::run_engine` yürütür. UI
//! yalnızca paylaşılan durumu (`Shared`) okur ve `UiCommand` gönderir; ağ mantığı içermez.

use crate::frame::FrameBuffer;
use crate::input::{InputEvent, KeyCode, MouseButton};
use crate::net::{Screen, Shared, UiCommand};
use eframe::egui;
use tokio::sync::mpsc::UnboundedSender;

pub struct AwayApp {
    shared: Shared,
    cmd_tx: UnboundedSender<UiCommand>,
    frames: FrameBuffer,
    texture: Option<egui::TextureHandle>,
    // Form/giriş alanları (yalnızca UI tarafında yaşar)
    f_server: String,
    f_user: String,
    f_pass: String,
    /// Kayıt ekranındaki "şifre tekrar" alanı.
    f_pass2: String,
    f_peer: String,
    // Otomatik bağlan (argümanla geldiyse), giriş sonrası bir kez tetiklenir
    auto_peer: Option<String>,
    auto_done: bool,

    // --- uzaktan giriş (viewer) ---
    /// Fare/klavye uzak makineye gönderilsin mi (araç çubuğundaki anahtar).
    control: bool,
    /// En son karşıya bildirilen değiştirici tuş durumu. egui Ctrl/Shift/Alt için ayrı
    /// tuş olayı üretmediğinden bas/bırak olaylarını bu farktan türetiyoruz.
    mods: egui::Modifiers,
    /// Tekerlek artığı: egui ham delta verir, enigo tam "klik" ister.
    scroll: egui::Vec2,
    /// En son gönderilen normalize imleç konumu (aynı noktayı tekrar göndermemek için).
    last_pos: Option<(f32, f32)>,
}

impl AwayApp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        shared: Shared,
        cmd_tx: UnboundedSender<UiCommand>,
        frames: FrameBuffer,
        server: String,
        user: String,
        pass: String,
        auto_peer: Option<String>,
    ) -> Self {
        Self {
            shared,
            cmd_tx,
            frames,
            texture: None,
            f_server: server,
            f_user: user,
            f_pass: pass,
            f_pass2: String::new(),
            f_peer: String::new(),
            auto_peer,
            auto_done: false,
            control: true,
            mods: egui::Modifiers::default(),
            scroll: egui::Vec2::ZERO,
            last_pos: None,
        }
    }

    fn send(&self, cmd: UiCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Message ekranından Home'a dönüş (saf UI geçişi).
    fn go_home(&self) {
        let mut s = self.shared.lock().unwrap();
        s.screen = Screen::Home;
        s.status = "hazır".into();
    }

    /// Giriş <-> Hesap oluştur geçişi (motoru ilgilendirmeyen saf UI geçişi).
    fn go_screen(&self, screen: Screen, status: &str) {
        let mut s = self.shared.lock().unwrap();
        s.screen = screen;
        s.status = status.into();
    }
}

impl eframe::App for AwayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Durum anlık görüntüsü (kilidi hemen bırak).
        let (screen, my_username, status) = {
            let s = self.shared.lock().unwrap();
            (s.screen.clone(), s.my_username.clone(), s.status.clone())
        };

        // Uzak ekrandan çıkıldıysa basılı sayılan değiştiricileri bırak; yoksa karşı
        // makinede Ctrl/Shift/Alt asılı kalır ve makine kullanılamaz hâle gelir.
        if !matches!(screen, Screen::RemoteScreen { .. }) {
            self.release_modifiers();
        }

        // Giriş yapıldıysa ve argümanla otomatik bağlanılacaksa bir kez tetikle.
        if !self.auto_done {
            if let (Some(peer), Some(_)) = (self.auto_peer.clone(), &my_username) {
                if matches!(screen, Screen::Home) {
                    self.f_peer = peer.clone();
                    self.send(UiCommand::Connect { to: peer });
                    self.auto_done = true;
                }
            }
        }

        // Üst durum çubuğu
        egui::TopBottomPanel::top("durum").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong("aWay");
                if let Some(u) = &my_username {
                    ui.separator();
                    ui.label(format!("Sen: {u}"));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(&status).weak());
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match screen {
            Screen::Login => self.ui_login(ui),
            Screen::Register => self.ui_register(ui),
            Screen::Reconnecting => self.ui_reconnecting(ui),
            Screen::Home => self.ui_home(ui, my_username.as_deref()),
            Screen::Connecting { peer } => self.ui_connecting(ui, &peer),
            Screen::Incoming { from } => self.ui_incoming(ui, &from),
            Screen::RemoteScreen { peer } => self.ui_remote(ui, ctx, &peer),
            Screen::Sharing { peer } => self.ui_sharing(ui, &peer),
            Screen::Message { text, error } => self.ui_message(ui, &text, error),
        });
    }
}

impl AwayApp {
    fn ui_login(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.heading("aWay — Giriş");
            ui.add_space(16.0);
        });
        egui::Grid::new("giris").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            ui.label("Sunucu");
            ui.add(egui::TextEdit::singleline(&mut self.f_server).desired_width(280.0));
            ui.end_row();
            ui.label("Kullanıcı");
            ui.add(egui::TextEdit::singleline(&mut self.f_user).desired_width(280.0));
            ui.end_row();
            ui.label("Şifre");
            ui.add(egui::TextEdit::singleline(&mut self.f_pass).password(true).desired_width(280.0));
            ui.end_row();
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(egui::RichText::new("Giriş yap").strong())).clicked() {
                self.send(UiCommand::Login {
                    server: self.f_server.clone(),
                    user: self.f_user.clone(),
                    pass: self.f_pass.clone(),
                });
            }
            ui.add_space(8.0);
            if ui.button("Hesap oluştur").clicked() {
                self.f_pass2.clear();
                self.go_screen(Screen::Register, "yeni hesap");
            }
        });
    }

    fn ui_register(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.heading("aWay — Hesap oluştur");
            ui.add_space(16.0);
        });
        egui::Grid::new("kayit").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            ui.label("Sunucu");
            ui.add(egui::TextEdit::singleline(&mut self.f_server).desired_width(280.0));
            ui.end_row();
            ui.label("Kullanıcı");
            ui.add(
                egui::TextEdit::singleline(&mut self.f_user)
                    .hint_text("bağlanırken yazılacak ad")
                    .desired_width(280.0),
            );
            ui.end_row();
            ui.label("Şifre");
            ui.add(egui::TextEdit::singleline(&mut self.f_pass).password(true).desired_width(280.0));
            ui.end_row();
            ui.label("Şifre (tekrar)");
            ui.add(egui::TextEdit::singleline(&mut self.f_pass2).password(true).desired_width(280.0));
            ui.end_row();
        });

        // Sunucuya gitmeden önce basit doğrulama; hata mesajı durum çubuğunda gösterilir.
        let user = self.f_user.trim().to_string();
        let problem = if user.is_empty() {
            Some("kullanıcı adı boş olamaz")
        } else if self.f_pass.is_empty() {
            Some("şifre boş olamaz")
        } else if self.f_pass != self.f_pass2 {
            Some("şifreler eşleşmiyor")
        } else {
            None
        };

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(egui::RichText::new("Hesabı oluştur").strong())).clicked() {
                match problem {
                    Some(msg) => self.go_screen(Screen::Register, msg),
                    None => self.send(UiCommand::Register {
                        server: self.f_server.clone(),
                        user,
                        pass: self.f_pass.clone(),
                    }),
                }
            }
            ui.add_space(8.0);
            if ui.button("Girişe dön").clicked() {
                self.go_screen(Screen::Login, "");
            }
        });
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Hesap açıldıktan sonra otomatik giriş yapılır.").weak(),
        );
    }

    fn ui_home(&mut self, ui: &mut egui::Ui, me: Option<&str>) {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            if let Some(u) = me {
                ui.label(egui::RichText::new(format!("Kullanıcı adın: {u}")).size(18.0));
                ui.label(egui::RichText::new("Gelen bağlantılar dinleniyor…").weak());
            }
            ui.add_space(24.0);
            ui.heading("Uzak masaüstüne bağlan");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 320.0).max(0.0) / 2.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.f_peer)
                        .hint_text("kullanıcı adı")
                        .desired_width(220.0),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Bağlan").clicked() || enter) && !self.f_peer.trim().is_empty() {
                    self.send(UiCommand::Connect { to: self.f_peer.trim().to_string() });
                }
            });
        });
    }

    fn ui_reconnecting(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.add(egui::Spinner::new().size(32.0));
            ui.add_space(12.0);
            ui.heading("Sunucu bağlantısı koptu");
            ui.label("Otomatik yeniden bağlanılıyor…");
        });
    }

    fn ui_connecting(&mut self, ui: &mut egui::Ui, peer: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.add(egui::Spinner::new().size(32.0));
            ui.add_space(12.0);
            ui.label(format!("{peer} bağlanılıyor…"));
            ui.add_space(16.0);
            if ui.button("İptal").clicked() {
                self.send(UiCommand::Hangup);
            }
        });
    }

    fn ui_incoming(&mut self, ui: &mut egui::Ui, from: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.heading("Gelen bağlantı");
            ui.add_space(8.0);
            ui.label(egui::RichText::new(format!("{from}")).size(20.0).strong());
            ui.label("ekranını görmek istiyor.");
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 220.0).max(0.0) / 2.0);
                if ui.add(egui::Button::new(egui::RichText::new("Kabul et").strong())).clicked() {
                    self.send(UiCommand::Accept);
                }
                ui.add_space(12.0);
                if ui.button("Reddet").clicked() {
                    self.send(UiCommand::Reject);
                }
            });
        });
    }

    fn ui_sharing(&mut self, ui: &mut egui::Ui, peer: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.add(egui::Spinner::new().size(24.0));
            ui.add_space(12.0);
            ui.heading("Ekranın paylaşılıyor");
            ui.label(format!("{peer} ekranını izliyor."));
            ui.add_space(16.0);
            if ui.button("Paylaşımı durdur").clicked() {
                self.send(UiCommand::Hangup);
            }
        });
    }

    fn ui_message(&mut self, ui: &mut egui::Ui, text: &str, error: bool) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            let color = if error { egui::Color32::LIGHT_RED } else { egui::Color32::LIGHT_GREEN };
            ui.label(egui::RichText::new(text).size(18.0).color(color));
            ui.add_space(16.0);
            if ui.button("Tamam").clicked() {
                self.go_home();
            }
        });
    }

    fn ui_remote(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, peer: &str) {
        // Yeni kare varsa texture'a yükle.
        if let Some(frame) = self.frames.take() {
            let image =
                egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.data);
            match &mut self.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("uzak-ekran", image, egui::TextureOptions::LINEAR)),
            }
        }

        // Üst ince şerit: kontrol anahtarı + bağlantıyı kes.
        // Düğme durumları yerel değişkenlere alınıyor: kapanış içinde hem `&mut self.control`
        // hem `self.send(..)` kullanmak ödünç alma çakışması olurdu.
        let mut control = self.control;
        let mut hangup = false;
        egui::TopBottomPanel::top("uzak-arac").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("🖥 {peer}"));
                ui.separator();
                ui.checkbox(&mut control, "Kontrol")
                    .on_hover_text("Fare ve klavyen uzak makineye gönderilsin");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Bağlantıyı kes").clicked() {
                        hangup = true;
                    }
                });
            });
        });
        if control != self.control {
            self.control = control;
            if !control {
                self.release_modifiers();
            }
        }
        if hangup {
            self.send(UiCommand::Hangup);
        }

        // Görüntü en-boy oranı korunarak yerleştirilir ve dikdörtgeni dışarı taşınır:
        // giriş eşlemesi bunu kullanır (siyah letterbox alanına tıklama uzak ekrana gitmemeli).
        let tex = self.texture.as_ref().map(|t| (t.id(), t.size_vec2()));
        let mut image_rect = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::BLACK))
            .show_inside(ui, |ui| {
                let area = ui.max_rect();
                let Some((id, size)) = tex else {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(egui::Color32::LIGHT_GRAY, "video bekleniyor…");
                    });
                    return;
                };
                if size.x <= 0.0 || size.y <= 0.0 {
                    return;
                }
                let scale = (area.width() / size.x).min(area.height() / size.y);
                let rect = egui::Rect::from_center_size(area.center(), size * scale);
                let sized = egui::load::SizedTexture::new(id, size);
                egui::Image::new(egui::ImageSource::Texture(sized)).paint_at(ui, rect);
                image_rect = Some(rect);
            });

        if let Some(rect) = image_rect {
            if self.control {
                self.pump_input(ctx, rect);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Uzaktan giriş: egui olayları -> InputEvent
// ---------------------------------------------------------------------------

impl AwayApp {
    /// Pencereye gelen fare/klavye olaylarını uzak makineye ilet.
    ///
    /// `rect` uzak ekran görüntüsünün penceredeki dikdörtgeni; konumlar buna göre 0..1'e
    /// normalize edilir, piksele çevirmeyi host kendi çözünürlüğüyle yapar.
    fn pump_input(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let (events, mods) = ctx.input(|i| (i.events.clone(), i.modifiers));
        self.sync_modifiers(mods);

        let norm = |p: egui::Pos2| -> Option<(f32, f32)> {
            if rect.width() <= 0.0 || rect.height() <= 0.0 || !rect.contains(p) {
                return None;
            }
            Some(((p.x - rect.left()) / rect.width(), (p.y - rect.top()) / rect.height()))
        };

        for ev in events {
            match ev {
                egui::Event::PointerMoved(p) => {
                    if let Some(pos) = norm(p) {
                        // Aynı noktayı tekrar göndermek boşuna paket; 60 Hz'de birikir.
                        if self.last_pos != Some(pos) {
                            self.last_pos = Some(pos);
                            self.send(UiCommand::Input(InputEvent::Move { x: pos.0, y: pos.1 }));
                        }
                    }
                }
                egui::Event::PointerButton { pos, button, pressed, .. } => {
                    if let (Some((x, y)), Some(b)) = (norm(pos), to_mouse_button(button)) {
                        self.last_pos = Some((x, y));
                        self.send(UiCommand::Input(InputEvent::Button { b, x, y, down: pressed }));
                    }
                }
                egui::Event::MouseWheel { unit, delta, .. } => self.push_scroll(unit, delta),
                // Yazılabilir karakterler buradan gider: klavye düzenini İZLEYİCİ çözer,
                // böylece Türkçe harfler host'un düzeninden bağımsız olarak doğru gelir.
                egui::Event::Text(s) => self.send(UiCommand::Input(InputEvent::Text { s })),
                egui::Event::Key { key, pressed, repeat, modifiers, .. } => {
                    self.forward_key(key, pressed, repeat, modifiers)
                }
                _ => {}
            }
        }
    }

    /// Değiştirici tuşların bas/bırak olaylarını durum farkından türet.
    fn sync_modifiers(&mut self, now: egui::Modifiers) {
        let was = self.mods;
        for (down, before, k) in [
            (now.ctrl, was.ctrl, KeyCode::Ctrl),
            (now.shift, was.shift, KeyCode::Shift),
            (now.alt, was.alt, KeyCode::Alt),
        ] {
            if down != before {
                self.send(UiCommand::Input(InputEvent::Key { k, down }));
            }
        }
        self.mods = now;
    }

    /// Basılı sayılan tüm değiştiricileri bırak (kontrol kapatıldı / uzak ekrandan çıkıldı).
    fn release_modifiers(&mut self) {
        if self.mods != egui::Modifiers::default() {
            self.sync_modifiers(egui::Modifiers::default());
        }
        self.last_pos = None;
        self.scroll = egui::Vec2::ZERO;
    }

    /// Tekerlek deltasını "klik" birimine çevirip biriktir, tam klik oluştukça gönder.
    ///
    /// İşaretler ters: egui'de pozitif delta İÇERİĞİN sağa/aşağı kaymasıdır, yani tekerlek
    /// yukarı/sola dönmüştür; enigo ise pozitifi aşağı/sağa sayar.
    fn push_scroll(&mut self, unit: egui::MouseWheelUnit, delta: egui::Vec2) {
        let clicks_per_unit = match unit {
            egui::MouseWheelUnit::Line => 1.0,
            egui::MouseWheelUnit::Page => 8.0,
            egui::MouseWheelUnit::Point => 1.0 / 50.0,
        };
        self.scroll -= delta * clicks_per_unit;
        let (dx, dy) = (self.scroll.x.trunc(), self.scroll.y.trunc());
        if dx != 0.0 || dy != 0.0 {
            self.scroll -= egui::vec2(dx, dy);
            self.send(UiCommand::Input(InputEvent::Scroll { dx: dx as i32, dy: dy as i32 }));
        }
    }

    /// Tuş olayını ilet.
    ///
    /// Yazılabilir karakterler zaten `Event::Text` ile gidiyor; onları burada tekrar
    /// göndermek her harfi çift yazardı. Tek istisna kısayollar: Ctrl/Alt basılıyken
    /// metin olayı üretilmez, o yüzden karakter tuşunun kendisini iletiriz.
    fn forward_key(&self, key: egui::Key, pressed: bool, repeat: bool, mods: egui::Modifiers) {
        // Tekrar olayları gereksiz: tuş uzakta zaten basılı, otomatik tekrarı o makine üretir.
        if repeat {
            return;
        }
        if let Some(k) = to_key_code(key) {
            self.send(UiCommand::Input(InputEvent::Key { k, down: pressed }));
        } else if mods.ctrl || mods.alt || mods.mac_cmd {
            if let Some(c) = to_char(key) {
                self.send(UiCommand::Input(InputEvent::Char { c, down: pressed }));
            }
        }
    }
}

fn to_mouse_button(b: egui::PointerButton) -> Option<MouseButton> {
    match b {
        egui::PointerButton::Primary => Some(MouseButton::Left),
        egui::PointerButton::Secondary => Some(MouseButton::Right),
        egui::PointerButton::Middle => Some(MouseButton::Middle),
        egui::PointerButton::Extra1 => Some(MouseButton::Back),
        egui::PointerButton::Extra2 => Some(MouseButton::Forward),
    }
}

/// Karakter üretmeyen tuşlar. Buraya girmeyenler `Event::Text` ya da kısayol yolundan gider.
///
/// `Space` bilerek yok: egui boşluk için hem `Key::Space` hem `Text(" ")` üretir, ikisini de
/// iletmek çift boşluk yazar.
fn to_key_code(key: egui::Key) -> Option<KeyCode> {
    use egui::Key as K;
    Some(match key {
        K::Enter => KeyCode::Enter,
        K::Tab => KeyCode::Tab,
        K::Backspace => KeyCode::Backspace,
        K::Delete => KeyCode::Delete,
        K::Escape => KeyCode::Escape,
        K::Insert => KeyCode::Insert,
        K::ArrowUp => KeyCode::Up,
        K::ArrowDown => KeyCode::Down,
        K::ArrowLeft => KeyCode::Left,
        K::ArrowRight => KeyCode::Right,
        K::Home => KeyCode::Home,
        K::End => KeyCode::End,
        K::PageUp => KeyCode::PageUp,
        K::PageDown => KeyCode::PageDown,
        K::F1 => KeyCode::F1,
        K::F2 => KeyCode::F2,
        K::F3 => KeyCode::F3,
        K::F4 => KeyCode::F4,
        K::F5 => KeyCode::F5,
        K::F6 => KeyCode::F6,
        K::F7 => KeyCode::F7,
        K::F8 => KeyCode::F8,
        K::F9 => KeyCode::F9,
        K::F10 => KeyCode::F10,
        K::F11 => KeyCode::F11,
        K::F12 => KeyCode::F12,
        _ => return None,
    })
}

/// Kısayollarda kullanılan karakter tuşları (Ctrl+C, Alt+F4 …).
fn to_char(key: egui::Key) -> Option<char> {
    use egui::Key as K;
    Some(match key {
        K::A => 'a', K::B => 'b', K::C => 'c', K::D => 'd', K::E => 'e', K::F => 'f',
        K::G => 'g', K::H => 'h', K::I => 'i', K::J => 'j', K::K => 'k', K::L => 'l',
        K::M => 'm', K::N => 'n', K::O => 'o', K::P => 'p', K::Q => 'q', K::R => 'r',
        K::S => 's', K::T => 't', K::U => 'u', K::V => 'v', K::W => 'w', K::X => 'x',
        K::Y => 'y', K::Z => 'z',
        K::Num0 => '0', K::Num1 => '1', K::Num2 => '2', K::Num3 => '3', K::Num4 => '4',
        K::Num5 => '5', K::Num6 => '6', K::Num7 => '7', K::Num8 => '8', K::Num9 => '9',
        K::Space => ' ',
        K::Minus => '-', K::Equals => '=', K::Plus => '+', K::Comma => ',', K::Period => '.',
        K::Slash => '/', K::Backslash => '\\', K::Semicolon => ';', K::Quote => '\'',
        K::OpenBracket => '[', K::CloseBracket => ']', K::Backtick => '`',
        _ => return None,
    })
}
