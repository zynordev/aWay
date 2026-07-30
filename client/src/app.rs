//! Tek pencereli eframe arayüzü (AnyDesk tarzı) — `media` feature.
//!
//! Ana thread'de çalışır; ağ/webrtc işini arka plandaki `net::run_engine` yürütür. UI
//! yalnızca paylaşılan durumu (`Shared`) okur ve `UiCommand` gönderir; ağ mantığı içermez.

use crate::frame::FrameBuffer;
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

        // Üst ince şerit: bağlantıyı kes.
        egui::TopBottomPanel::top("uzak-arac").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("🖥 {peer}"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Bağlantıyı kes").clicked() {
                        self.send(UiCommand::Hangup);
                    }
                });
            });
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::BLACK))
            .show_inside(ui, |ui| {
                if let Some(tex) = &self.texture {
                    let sized = egui::load::SizedTexture::new(tex.id(), tex.size_vec2());
                    let source = egui::ImageSource::Texture(sized);
                    ui.centered_and_justified(|ui| {
                        ui.add(egui::Image::new(source).fit_to_exact_size(ui.available_size()));
                    });
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(egui::Color32::LIGHT_GRAY, "video bekleniyor…");
                    });
                }
            });
    }
}
