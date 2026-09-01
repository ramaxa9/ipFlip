#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

const ICON_ICO: &[u8] = include_bytes!("../icon.ico");

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui::{self, Color32, CornerRadius, Frame, RichText, Stroke, Vec2};
use eframe::{App, CreationContext, NativeOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::Shell::IsUserAnAdmin;
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// ---------- Palette ----------
// A calm, modern dark theme: deep slate background, layered surfaces,
// an indigo primary accent, and clearly differentiated action colors.
mod palette {
    use eframe::egui::Color32;

    pub const BG: Color32 = Color32::from_rgb(13, 15, 20);
    pub const SURFACE: Color32 = Color32::from_rgb(19, 22, 30);
    pub const SURFACE_ALT: Color32 = Color32::from_rgb(24, 28, 38);
    pub const ELEVATED: Color32 = Color32::from_rgb(30, 35, 47);
    pub const BORDER: Color32 = Color32::from_rgb(38, 43, 56);

    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(232, 234, 240);
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(148, 155, 175);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(96, 102, 122);

    pub const ACCENT: Color32 = Color32::from_rgb(129, 140, 248); // indigo-400
    pub const ACCENT_STRONG: Color32 = Color32::from_rgb(99, 102, 241); // indigo-500

    pub const SUCCESS: Color32 = Color32::from_rgb(52, 211, 153); // emerald-400
    pub const SUCCESS_BG: Color32 = Color32::from_rgb(20, 40, 36);
    pub const INFO: Color32 = Color32::from_rgb(56, 189, 248); // sky-400
    pub const WARNING: Color32 = Color32::from_rgb(251, 191, 36); // amber-400
    pub const DANGER: Color32 = Color32::from_rgb(248, 113, 113); // red-400
    pub const DANGER_BG: Color32 = Color32::from_rgb(43, 22, 24);
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IpProfile {
    #[serde(default)]
    name: String,
    net_interface: String,
    ip: String,
    mask: String,
    gateway: String,
}

#[derive(Clone, Debug, Default)]
struct InterfaceSettings {
    ip: String,
    mask: String,
    gateway: String,
    dhcp_enabled: Option<bool>,
}

enum WorkerMessage {
    InterfacesLoaded {
        interfaces: Vec<String>,
        categories: HashMap<String, u8>,
        device_names: HashMap<String, String>,
    },
    SettingsLoaded {
        interface_name: String,
        settings: Option<InterfaceSettings>,
        request_id: u64,
        fill_form: bool,
    },
    ApplyCompleted {
        interface_name: String,
        result: Result<(), String>,
        profile_to_save: Option<IpProfile>,
        success_message: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusTone {
    Neutral,
    Success,
    Danger,
}

struct IpFlipRustApp {
    profiles: Vec<IpProfile>,
    interfaces: Vec<String>,
    interface_category_by_name: HashMap<String, u8>,
    interface_device_name_by_name: HashMap<String, String>,
    selected_interface: String,
    preset_name_text: String,
    ip_text: String,
    mask_text: String,
    gateway_text: String,
    current_settings_message: String,
    status_message: String,
    status_tone: StatusTone,
    request_id: u64,
    interfaces_loading: bool,
    interface_details_loading: bool,
    tx: Sender<WorkerMessage>,
    rx: Receiver<WorkerMessage>,
    profiles_path: PathBuf,
}

impl IpFlipRustApp {
    fn new(_cc: &CreationContext<'_>) -> Self {
        let profiles_path = app_data_dir().join("ip_profiles.json");
        let profiles = load_profiles(&profiles_path);

        let selected_interface = profiles
            .first()
            .map(|p| p.net_interface.clone())
            .unwrap_or_default();

        let (tx, rx) = mpsc::channel();

        let selected_profile_name = profiles
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_default();

        let mut app = Self {
            profiles,
            interfaces: Vec::new(),
            interface_category_by_name: HashMap::new(),
            interface_device_name_by_name: HashMap::new(),
            selected_interface,
            preset_name_text: selected_profile_name,
            ip_text: String::new(),
            mask_text: String::new(),
            gateway_text: String::new(),
            current_settings_message: "Current settings will appear here.".to_string(),
            status_message: String::new(),
            status_tone: StatusTone::Neutral,
            request_id: 0,
            interfaces_loading: false,
            interface_details_loading: false,
            tx,
            rx,
            profiles_path,
        };

        if cfg!(target_os = "windows") && !is_windows_admin() {
            app.set_status(
                "Run the app as administrator before applying network settings.",
                StatusTone::Danger,
            );
        }

        app.reload_interfaces_async();
        if !app.selected_interface.is_empty() {
            app.request_interface_settings(app.selected_interface.clone(), true);
        }

        app
    }

    fn set_status(&mut self, message: impl Into<String>, tone: StatusTone) {
        self.status_message = message.into();
        self.status_tone = tone;
    }

    fn sync_preset_name_from_selected(&mut self) {
        let profile = self
            .profiles
            .iter()
            .find(|p| p.net_interface == self.selected_interface);
        self.preset_name_text = profile.map(|p| p.name.clone()).unwrap_or_default();
    }

    fn interface_selector_disabled(&self) -> bool {
        self.interfaces_loading || self.interface_details_loading
    }

    fn reload_interfaces_async(&mut self) {
        if self.interfaces_loading {
            return;
        }

        let mut from_profiles: Vec<String> = self
            .profiles
            .iter()
            .map(|p| p.net_interface.clone())
            .filter(|s| !s.trim().is_empty())
            .collect();

        from_profiles.sort_by_key(|n| {
            interface_sort_key_with_categories(&self.interface_category_by_name, n)
        });
        from_profiles.dedup();

        self.interfaces = from_profiles;
        if self.selected_interface.is_empty() && !self.interfaces.is_empty() {
            self.selected_interface = self.interfaces[0].clone();
        }

        self.interfaces_loading = true;
        self.set_status("Loading network interfaces...", StatusTone::Neutral);

        let tx = self.tx.clone();
        thread::spawn(move || {
            let interfaces = list_network_interfaces();
            let categories = load_interface_categories();
            let device_names = load_interface_device_names();
            let _ = tx.send(WorkerMessage::InterfacesLoaded {
                interfaces,
                categories,
                device_names,
            });
        });
    }

    fn request_interface_settings(&mut self, interface_name: String, fill_form: bool) {
        if interface_name.trim().is_empty() {
            self.current_settings_message = "Select an interface.".to_string();
            self.interface_details_loading = false;
            return;
        }

        self.request_id = self.request_id.saturating_add(1);
        let current_id = self.request_id;
        self.interface_details_loading = true;
        self.current_settings_message = format!("Reading {}...", interface_name);

        let tx = self.tx.clone();
        thread::spawn(move || {
            let settings = get_interface_ipv4_settings(&interface_name);
            let _ = tx.send(WorkerMessage::SettingsLoaded {
                interface_name,
                settings,
                request_id: current_id,
                fill_form,
            });
        });
    }

    fn apply_static_async(&mut self) {
        let interface = self.selected_interface.trim().to_string();
        let ip = self.ip_text.trim().to_string();
        let mask = self.mask_text.trim().to_string();
        let gateway = self.gateway_text.trim().to_string();

        if interface.is_empty() {
            self.set_status("Interface is required.", StatusTone::Danger);
            return;
        }
        if ip.is_empty() || mask.is_empty() || gateway.is_empty() {
            self.set_status(
                "For static mode, interface, ip, mask, and gateway are required.",
                StatusTone::Danger,
            );
            return;
        }
        if !is_ipv4(&ip) || !is_ipv4(&mask) || !is_ipv4(&gateway) {
            self.set_status(
                "IP, mask, and gateway must be valid IPv4 values.",
                StatusTone::Danger,
            );
            return;
        }

        self.set_status("Applying static IP...", StatusTone::Neutral);
        let tx = self.tx.clone();
        let profile = IpProfile {
            name: self.preset_name_text.trim().to_string(),
            net_interface: interface.clone(),
            ip: ip.clone(),
            mask: mask.clone(),
            gateway: gateway.clone(),
        };

        thread::spawn(move || {
            let result = change_ip_address(&interface, Some(&ip), Some(&mask), Some(&gateway));
            let _ = tx.send(WorkerMessage::ApplyCompleted {
                interface_name: interface,
                result,
                profile_to_save: Some(profile),
                success_message: "Static IP applied and saved.".to_string(),
            });
        });
    }

    fn apply_dhcp_async(&mut self) {
        let interface = if self.selected_interface.trim().is_empty() {
            "Ethernet".to_string()
        } else {
            self.selected_interface.trim().to_string()
        };

        self.set_status("Applying DHCP...", StatusTone::Neutral);
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = change_ip_address(&interface, None, None, None);
            let _ = tx.send(WorkerMessage::ApplyCompleted {
                interface_name: interface,
                result,
                profile_to_save: None,
                success_message: "Automatic IP (DHCP) applied.".to_string(),
            });
        });
    }

    fn save_profile(&mut self) {
        let interface = self.selected_interface.trim().to_string();
        let ip = self.ip_text.trim().to_string();
        let mask = self.mask_text.trim().to_string();
        let gateway = self.gateway_text.trim().to_string();

        if interface.is_empty() {
            self.set_status("Interface is required.", StatusTone::Danger);
            return;
        }
        if ip.is_empty() || mask.is_empty() || gateway.is_empty() {
            self.set_status(
                "To save, interface, ip, mask, and gateway are required.",
                StatusTone::Danger,
            );
            return;
        }
        if !is_ipv4(&ip) || !is_ipv4(&mask) || !is_ipv4(&gateway) {
            self.set_status(
                "IP, mask, and gateway must be valid IPv4 values.",
                StatusTone::Danger,
            );
            return;
        }

        let profile = IpProfile {
            name: self.preset_name_text.trim().to_string(),
            net_interface: interface.clone(),
            ip,
            mask,
            gateway,
        };

        self.add_profile(profile);
        self.set_status("Configuration saved.", StatusTone::Success);
    }

    fn add_profile(&mut self, profile: IpProfile) {
        if self.profiles.iter().any(|p| p == &profile) {
            return;
        }

        self.profiles.push(profile.clone());
        self.preset_name_text = profile.name.clone();
        save_profiles(&self.profiles_path, &self.profiles);

        if !profile.net_interface.is_empty() && !self.interfaces.contains(&profile.net_interface) {
            self.interfaces.push(profile.net_interface.clone());
            let categories = self.interface_category_by_name.clone();
            self.interfaces
                .sort_by_key(|n| interface_sort_key_with_categories(&categories, n));
        }
    }

    fn remove_profile(&mut self, profile: &IpProfile) {
        self.profiles.retain(|p| p != profile);
        save_profiles(&self.profiles_path, &self.profiles);
    }

    fn process_worker_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMessage::InterfacesLoaded {
                    mut interfaces,
                    categories,
                    device_names,
                } => {
                    self.interfaces_loading = false;
                    self.interface_category_by_name = categories;
                    self.interface_device_name_by_name = device_names;

                    for p in &self.profiles {
                        if !p.net_interface.is_empty() && !interfaces.contains(&p.net_interface) {
                            interfaces.push(p.net_interface.clone());
                        }
                    }

                    interfaces.sort_by_key(|n| {
                        interface_sort_key_with_categories(&self.interface_category_by_name, n)
                    });
                    interfaces.dedup();
                    self.interfaces = interfaces;

                    if self.selected_interface.is_empty() && !self.interfaces.is_empty() {
                        self.selected_interface = self.interfaces[0].clone();
                        self.request_interface_settings(self.selected_interface.clone(), true);
                    }

                    if self.status_message == "Loading network interfaces..." {
                        self.status_message.clear();
                    }
                }
                WorkerMessage::SettingsLoaded {
                    interface_name,
                    settings,
                    request_id,
                    fill_form,
                } => {
                    if request_id != self.request_id {
                        continue;
                    }

                    if self.selected_interface != interface_name {
                        continue;
                    }

                    self.interface_details_loading = false;

                    if let Some(settings) = settings {
                        if fill_form {
                            self.ip_text = settings.ip.clone();
                            self.mask_text = settings.mask.clone();
                            self.gateway_text = settings.gateway.clone();
                        }

                        let mode = if settings.dhcp_enabled.unwrap_or(false) {
                            "DHCP"
                        } else {
                            "Static"
                        };

                        let ip_text = if settings.ip.is_empty() {
                            "Unassigned".to_string()
                        } else {
                            settings.ip
                        };
                        let mask_text = if settings.mask.is_empty() {
                            "Unassigned".to_string()
                        } else {
                            settings.mask
                        };
                        let gateway_text = if settings.gateway.is_empty() {
                            "Unassigned".to_string()
                        } else {
                            settings.gateway
                        };

                        self.current_settings_message = format!(
                            "{} · {}\nIP {}\nMask {}\nGateway {}",
                            interface_name, mode, ip_text, mask_text, gateway_text
                        );
                    } else {
                        self.current_settings_message =
                            format!("{}: unavailable", interface_name);
                    }
                }
                WorkerMessage::ApplyCompleted {
                    interface_name,
                    result,
                    profile_to_save,
                    success_message,
                } => match result {
                    Ok(()) => {
                        if let Some(profile) = profile_to_save {
                            self.add_profile(profile);
                        }
                        self.set_status(success_message, StatusTone::Success);
                        self.request_interface_settings(interface_name, true);
                    }
                    Err(err) => {
                        self.set_status(err, StatusTone::Danger);
                    }
                },
            }
        }
    }
}

// ---------- UI helpers ----------

fn section_frame() -> Frame {
    Frame::new()
        .fill(palette::SURFACE)
        .stroke(Stroke::new(1.0_f32, palette::BORDER))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::same(18))
}

fn pill_button_with_icon(icon: &str, text: &str, fill: Color32, text_color: Color32) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(format!("{} {}", icon, text))
            .strong()
            .color(text_color)
            .size(14.0),
    )
    .fill(fill)
    .corner_radius(CornerRadius::same(8))
    .min_size(Vec2::new(0.0, 36.0))
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .color(palette::TEXT_MUTED)
            .size(12.0)
            .strong(),
    );
}

fn status_colors(tone: StatusTone) -> (Color32, Color32) {
    match tone {
        StatusTone::Neutral => (palette::SURFACE_ALT, palette::TEXT_SECONDARY),
        StatusTone::Success => (palette::SUCCESS_BG, palette::SUCCESS),
        StatusTone::Danger => (palette::DANGER_BG, palette::DANGER),
    }
}

impl App for IpFlipRustApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_worker_messages();

        // ---------- Global style ----------
        let mut style = (*ctx.style()).clone();
        style.visuals.window_fill = palette::BG;
        style.visuals.panel_fill = palette::BG;
        style.visuals.override_text_color = Some(palette::TEXT_PRIMARY);
        style.visuals.widgets.noninteractive.bg_fill = palette::SURFACE;
        style.visuals.widgets.inactive.bg_fill = palette::SURFACE_ALT;
        style.visuals.widgets.inactive.weak_bg_fill = palette::SURFACE_ALT;
        style.visuals.widgets.hovered.bg_fill = palette::ELEVATED;
        style.visuals.widgets.hovered.weak_bg_fill = palette::ELEVATED;
        style.visuals.widgets.active.bg_fill = palette::ELEVATED;
        style.visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
        style.visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
        style.visuals.widgets.active.corner_radius = CornerRadius::same(8);
        style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, palette::BORDER);
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette::ACCENT);
        style.visuals.selection.bg_fill = palette::ACCENT_STRONG.linear_multiply(0.55);
        style.visuals.selection.stroke = Stroke::new(1.0_f32, palette::ACCENT);
        style.spacing.item_spacing = Vec2::new(10.0, 10.0);
        style.spacing.button_padding = Vec2::new(14.0, 8.0);
        ctx.set_style(style);

        // ---------- Header ----------
        egui::TopBottomPanel::top("header")
            .frame(
                Frame::new()
                    .fill(palette::BG)
                    .inner_margin(egui::Margin::symmetric(20, 16)),
            )
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("ipFlip")
                                .color(palette::TEXT_PRIMARY)
                                .size(22.0)
                                .strong(),
                        );
                        ui.label(
                            RichText::new("Network profile manager")
                                .color(palette::TEXT_MUTED)
                                .size(13.0),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let admin = !cfg!(target_os = "windows") || is_windows_admin();
                        let (bg, fg, text) = if admin {
                            (palette::SUCCESS_BG, palette::SUCCESS, "Administrator")
                        } else {
                            (palette::DANGER_BG, palette::DANGER, "Not elevated")
                        };
                        Frame::new()
                            .fill(bg)
                            .corner_radius(CornerRadius::same(20))
                            .inner_margin(egui::Margin::symmetric(12, 6))
                            .show(ui, |ui| {
                                ui.label(RichText::new(text).color(fg).size(12.0).strong());
                            });
                    });
                });
            });

        // ---------- Body ----------
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(palette::BG)
                    .inner_margin(egui::Margin::symmetric(20, 4)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.columns(2, |columns| {
                            section_frame().show(&mut columns[0], |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Saved Profiles")
                                            .color(palette::TEXT_PRIMARY)
                                            .size(17.0)
                                            .strong(),
                                    );
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        Frame::new()
                                            .fill(palette::SURFACE_ALT)
                                            .corner_radius(CornerRadius::same(10))
                                            .inner_margin(egui::Margin::symmetric(8, 3))
                                            .show(ui, |ui| {
                                                ui.label(
                                                    RichText::new(format!("{}", self.profiles.len()))
                                                        .color(palette::TEXT_SECONDARY)
                                                        .size(12.0)
                                                        .strong(),
                                                );
                                            });
                                    });
                                });
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new("Click to load into the form · double-click to apply")
                                        .color(palette::TEXT_MUTED)
                                        .size(11.5),
                                );
                                ui.add_space(10.0);

                                egui::ScrollArea::vertical()
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        if self.profiles.is_empty() {
                                            ui.add_space(20.0);
                                            ui.vertical_centered(|ui| {
                                                ui.label(
                                                    RichText::new("No saved profiles yet")
                                                        .color(palette::TEXT_MUTED)
                                                        .italics(),
                                                );
                                            });
                                        }

                                        let mut profile_to_delete: Option<IpProfile> = None;

                                        for (idx, profile) in self.profiles.clone().into_iter().enumerate() {
                                            ui.push_id(idx, |ui| {
                                                let mut delete_clicked = false;
                                                let mut name_field_active = false;
                                                let row_height = 68.0;

                                                let frame_response = Frame::new()
                                                    .fill(palette::SURFACE_ALT)
                                                    .stroke(Stroke::new(1.0_f32, palette::BORDER))
                                                    .corner_radius(CornerRadius::same(10))
                                                    .inner_margin(egui::Margin::symmetric(12, 10))
                                                    .show(ui, |ui| {
                                                        ui.set_min_height(row_height);
                                                        ui.horizontal(|ui| {
                                                            ui.with_layout(
                                                                egui::Layout::right_to_left(egui::Align::Center),
                                                                |ui| {
                                                                    let delete_btn = egui::Button::new(
                                                                        RichText::new("🗑")
                                                                            .color(palette::DANGER)
                                                                            .size(13.0),
                                                                    )
                                                                    .fill(palette::DANGER_BG)
                                                                    .corner_radius(CornerRadius::same(7));
                                                                    let delete_response = ui.add_sized([28.0, 28.0], delete_btn);
                                                                    if delete_response.clicked() {
                                                                        delete_clicked = true;
                                                                    }

                                                                    ui.vertical(|ui| {
                                                                        ui.set_width(ui.available_width());
                                                                        let mut editable_name = profile_label(&profile);
                                                                        let name_response = ui.add(
                                                                            egui::TextEdit::singleline(&mut editable_name)
                                                                                .desired_width(ui.available_width())
                                                                                .margin(egui::Margin::symmetric(12, 10))
                                                                                .text_color(palette::TEXT_PRIMARY)
                                                                                .hint_text(profile.net_interface.clone())
                                                                        );
                                                                        let name_rect = name_response.rect;
                                                                        let pointer_on_name_field = ui.ctx().input(|i| {
                                                                            i.pointer
                                                                                .hover_pos()
                                                                                .is_some_and(|pos| name_rect.contains(pos))
                                                                        });
                                                                        name_field_active = name_response.clicked()
                                                                            || name_response.double_clicked()
                                                                            || name_response.has_focus()
                                                                            || pointer_on_name_field;
                                                                        if name_response.changed() {
                                                                            let new_name = editable_name.trim().to_string();
                                                                            let normalized_name = if new_name == profile.net_interface {
                                                                                String::new()
                                                                            } else {
                                                                                new_name
                                                                            };
                                                                            if self.profiles.get_mut(idx).map(|p| p.name = normalized_name.clone()).is_some() {
                                                                                save_profiles(&self.profiles_path, &self.profiles);
                                                                                if self.selected_interface == profile.net_interface {
                                                                                    self.preset_name_text = normalized_name;
                                                                                }
                                                                            }
                                                                        }
                                                                        ui.add(
                                                                            egui::Label::new(
                                                                                RichText::new(&profile.net_interface)
                                                                                    .color(palette::TEXT_MUTED)
                                                                                    .size(11.0),
                                                                            )
                                                                            .truncate(),
                                                                        );
                                                                        ui.add(
                                                                            egui::Label::new(
                                                                                RichText::new(&profile.ip)
                                                                                    .color(palette::ACCENT)
                                                                                    .size(13.0)
                                                                                    .monospace(),
                                                                            )
                                                                            .truncate(),
                                                                        );
                                                                        ui.add(
                                                                            egui::Label::new(
                                                                                RichText::new(format!(
                                                                                    "mask {}  ·  gw {}",
                                                                                    profile.mask, profile.gateway
                                                                                ))
                                                                                .color(palette::TEXT_MUTED)
                                                                                .size(11.5)
                                                                                .monospace(),
                                                                            )
                                                                            .truncate(),
                                                                        );
                                                                    });
                                                                },
                                                            );
                                                        });
                                                    })
                                                    .response;

                                                let row_rect = egui::Rect::from_min_max(
                                                    frame_response.rect.min,
                                                    egui::pos2(frame_response.rect.right() - 40.0, frame_response.rect.bottom()),
                                                );

                                                if delete_clicked {
                                                    profile_to_delete = Some(profile.clone());
                                                } else {
                                                    let row = ui.interact(
                                                        row_rect,
                                                        ui.id().with("row"),
                                                        egui::Sense::click(),
                                                    );

                                                    if row.clicked() && !name_field_active {
                                                        self.selected_interface = profile.net_interface.clone();
                                                        self.preset_name_text = profile.name.clone();
                                                        self.ip_text = profile.ip.clone();
                                                        self.mask_text = profile.mask.clone();
                                                        self.gateway_text = profile.gateway.clone();
                                                        self.set_status(
                                                            "Loaded saved profile into form.",
                                                            StatusTone::Neutral,
                                                        );
                                                    }
                                                    if row.double_clicked() && !name_field_active {
                                                        self.selected_interface = profile.net_interface.clone();
                                                        self.preset_name_text = profile.name.clone();
                                                        self.ip_text = profile.ip.clone();
                                                        self.mask_text = profile.mask.clone();
                                                        self.gateway_text = profile.gateway.clone();
                                                        self.apply_static_async();
                                                    }
                                                }
                                            });
                                            ui.add_space(8.0);
                                        }

                                        if let Some(profile) = profile_to_delete {
                                            self.remove_profile(&profile);
                                            self.set_status("Profile deleted.", StatusTone::Neutral);
                                        }
                                    });
                            });

                            section_frame().show(&mut columns[1], |ui| {
                                ui.label(
                                    RichText::new("Network Settings")
                                        .color(palette::TEXT_PRIMARY)
                                        .size(17.0)
                                        .strong(),
                                );
                                ui.add_space(12.0);

                                field_label(ui, "INTERFACE");
                                ui.add_space(4.0);
                                let mut selected_changed = false;
                                let selector_disabled = self.interface_selector_disabled();
                                ui.add_enabled_ui(!selector_disabled, |ui| {
                                    egui::ComboBox::from_id_salt("interface_spinner")
                                        .selected_text(if self.selected_interface.is_empty() {
                                            "Select interface".to_string()
                                        } else {
                                            self.selected_interface.clone()
                                        })
                                        .width(ui.available_width())
                                        .show_ui(ui, |ui| {
                                            for interface in self.interfaces.clone() {
                                                let device_name = self
                                                    .interface_device_name_by_name
                                                    .get(&interface)
                                                    .cloned()
                                                    .unwrap_or_default();
                                                let label = if device_name.trim().is_empty() {
                                                    interface.clone()
                                                } else {
                                                    format!("{}\n{}", interface, device_name)
                                                };

                                                let response = ui.selectable_value(
                                                    &mut self.selected_interface,
                                                    interface.clone(),
                                                    RichText::new(label)
                                                        .color(palette::TEXT_PRIMARY)
                                                        .size(14.0),
                                                );
                                                if response.changed() {
                                                    selected_changed = true;
                                                }
                                            }
                                        });
                                });

                                if selected_changed {
                                    self.sync_preset_name_from_selected();
                                    self.request_interface_settings(self.selected_interface.clone(), true);
                                }

                                ui.add_space(12.0);

                                Frame::new()
                                    .fill(palette::SURFACE_ALT)
                                    .corner_radius(CornerRadius::same(10))
                                    .inner_margin(egui::Margin::symmetric(12, 10))
                                    .show(ui, |ui| {
                                        for (i, line) in self.current_settings_message.clone().lines().enumerate() {
                                            let text = if i == 0 {
                                                RichText::new(line).color(palette::TEXT_SECONDARY).strong().size(12.5)
                                            } else {
                                                RichText::new(line).color(palette::TEXT_MUTED).monospace().size(12.0)
                                            };
                                            ui.label(text);
                                        }
                                    });

                                ui.add_space(12.0);

                                field_label(ui, "PRESET NAME");
                                ui.add_space(4.0);
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.preset_name_text)
                                        .hint_text("")
                                        .desired_width(ui.available_width())
                                        .margin(egui::Margin::symmetric(10, 8)),
                                );

                                ui.add_space(14.0);

                                if self.mask_text.is_empty() && !self.ip_text.trim().is_empty() {
                                    self.mask_text = "255.255.255.0".to_string();
                                }

                                field_label(ui, "IP ADDRESS");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.ip_text)
                                        .hint_text("e.g. 192.168.1.50")
                                        .desired_width(ui.available_width())
                                        .margin(egui::Margin::symmetric(10, 8)),
                                );
                                ui.add_space(6.0);

                                field_label(ui, "SUBNET MASK");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.mask_text)
                                        .hint_text("e.g. 255.255.255.0")
                                        .desired_width(ui.available_width())
                                        .margin(egui::Margin::symmetric(10, 8)),
                                );
                                ui.add_space(6.0);

                                field_label(ui, "GATEWAY");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.gateway_text)
                                        .hint_text("e.g. 192.168.1.1")
                                        .desired_width(ui.available_width())
                                        .margin(egui::Margin::symmetric(10, 8)),
                                );

                                ui.add_space(14.0);
                                ui.horizontal(|ui| {
                                    let dark_on_accent = Color32::from_rgb(14, 16, 22);
                                    if ui
                                        .add(pill_button_with_icon("⚙", "Apply Static IP", palette::ACCENT, dark_on_accent))
                                        .clicked()
                                    {
                                        self.apply_static_async();
                                    }
                                    if ui
                                        .add(pill_button_with_icon("💾", "Save Profile", palette::INFO, dark_on_accent))
                                        .clicked()
                                    {
                                        self.save_profile();
                                    }
                                    if ui
                                        .add(pill_button_with_icon("🔄", "Use DHCP", palette::WARNING, dark_on_accent))
                                        .clicked()
                                    {
                                        self.apply_dhcp_async();
                                    }
                                });

                                ui.add_space(14.0);

                                if !self.status_message.is_empty() {
                                    let (bg, fg) = status_colors(self.status_tone);
                                    Frame::new()
                                        .fill(bg)
                                        .corner_radius(CornerRadius::same(8))
                                        .inner_margin(egui::Margin::symmetric(12, 8))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(self.status_message.clone())
                                                    .color(fg)
                                                    .size(12.5),
                                            );
                                        });
                                    ui.add_space(8.0);
                                }

                                ui.label(
                                    RichText::new(
                                        "Changing IP settings on Windows usually requires running as administrator.",
                                    )
                                    .color(palette::TEXT_MUTED)
                                    .size(11.0)
                                    .italics(),
                                );
                            });
                        });
                    });
            });

        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_top_center_position, IpFlipRustApp, IpProfile};
    use std::sync::mpsc;

    #[test]
    fn compute_top_center_position_places_window_near_screen_center_top() {
        let pos = compute_top_center_position(1040.0, 760.0, 1920, 1080);
        assert_eq!(pos, (440.0, 0.0));
    }

    #[test]
    fn interface_selector_is_disabled_while_reading_details() {
        let (tx, rx) = mpsc::channel();
        let app = IpFlipRustApp {
            profiles: vec![],
            interfaces: vec![],
            interface_category_by_name: Default::default(),
            interface_device_name_by_name: Default::default(),
            selected_interface: String::new(),
            ip_text: String::new(),
            mask_text: String::new(),
            gateway_text: String::new(),
            preset_name_text: String::new(),
            current_settings_message: String::new(),
            status_message: String::new(),
            status_tone: super::StatusTone::Neutral,
            request_id: 0,
            interfaces_loading: false,
            interface_details_loading: true,
            tx,
            rx,
            profiles_path: Default::default(),
        };

        assert!(app.interface_selector_disabled());
    }

    #[test]
    fn legacy_profile_json_without_name_deserializes() {
        let raw = r#"[{"net_interface":"Ethernet","ip":"192.168.1.10","mask":"255.255.255.0","gateway":"192.168.1.1"}]"#;
        let profiles: Vec<IpProfile> = serde_json::from_str(raw).unwrap();

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "");
    }
}

fn compute_top_center_position(window_width: f32, _window_height: f32, screen_width: u32, _screen_height: u32) -> (f32, f32) {
    let x = ((screen_width as f32 - window_width) / 2.0).max(0.0);
    let y = 0.0;
    (x, y)
}

fn primary_screen_size() -> (u32, u32) {
    #[cfg(target_os = "windows")]
    {
        let width = unsafe { GetSystemMetrics(0) } as u32;
        let height = unsafe { GetSystemMetrics(1) } as u32;
        return (width, height);
    }

    #[cfg(not(target_os = "windows"))]
    {
        (1920, 1080)
    }
}

fn main() {
    ensure_windows_elevation();

    let scale = system_scale_factor();
    let (w, h) = (1040.0 / scale, 760.0 / scale);
    let (screen_w, _screen_h) = primary_screen_size();
    let (x, y) = compute_top_center_position(w, h, screen_w, _screen_h);

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([w, h])
        .with_min_inner_size([w * 0.8, 520.0])
        .with_resizable(true)
        .with_position([x, y]);
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = NativeOptions {
        viewport,
        ..Default::default()
    };

    let _ = eframe::run_native(
        "ipFlip",
        native_options,
        Box::new(|cc| Ok(Box::new(IpFlipRustApp::new(cc)))),
    );
}

#[cfg(target_os = "windows")]
fn system_scale_factor() -> f32 {
    let dpi = unsafe { GetDpiForSystem() };
    dpi as f32 / 96.0
}

#[cfg(not(target_os = "windows"))]
fn system_scale_factor() -> f32 {
    1.0
}

fn load_window_icon() -> Option<egui::viewport::IconData> {
    let image = image::load_from_memory(ICON_ICO).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::viewport::IconData { rgba: image.into_raw(), width, height })
}

fn app_data_dir() -> PathBuf {
    let base = env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            directories::BaseDirs::new()
                .map(|b| b.home_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."))
        });

    let dir = base.join("ipFlip");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn load_profiles(path: &Path) -> Vec<IpProfile> {
    let data = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return Vec::new(),
    };

    let json: Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let arr = match json.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for item in arr {
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let interface_name = item
            .get("net_interface")
            .or_else(|| item.get("interface"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let ip = item
            .get("ip")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mask = item
            .get("mask")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let gateway = item
            .get("gateway")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        if interface_name.is_empty() || ip.is_empty() || mask.is_empty() || gateway.is_empty() {
            continue;
        }

        out.push(IpProfile {
            name,
            net_interface: interface_name,
            ip,
            mask,
            gateway,
        });
    }

    out
}

fn save_profiles(path: &Path, profiles: &[IpProfile]) {
    if let Ok(json) = serde_json::to_string_pretty(profiles) {
        let _ = fs::write(path, json);
    }
}

fn profile_label(profile: &IpProfile) -> String {
    let trimmed = profile.name.trim();
    if trimmed.is_empty() {
        profile.net_interface.clone()
    } else {
        trimmed.to_string()
    }
}

fn run_hidden_command(program: &str, args: &[&str]) -> Result<std::process::Output, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd.output().map_err(|e| format!("Failed to run {}: {}", program, e))
}

fn list_network_interfaces() -> Vec<String> {
    let output = match run_hidden_command("netsh", &["interface", "ipv4", "show", "interfaces"]) {
        Ok(v) => v,
        Err(_) => return vec!["Ethernet".to_string()],
    };

    if !output.status.success() {
        return vec!["Ethernet".to_string()];
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut interfaces = Vec::new();
    for line in stdout.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with("Idx") || stripped.starts_with("---") {
            continue;
        }

        let parts: Vec<&str> = stripped.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }

        let name = parts[4..].join(" ").trim().to_string();
        if !name.is_empty() && !interfaces.contains(&name) {
            interfaces.push(name);
        }
    }

    if interfaces.is_empty() {
        vec!["Ethernet".to_string()]
    } else {
        interfaces
    }
}

fn load_interface_categories() -> HashMap<String, u8> {
    let command = "Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue | Select-Object Name, HardwareInterface, InterfaceDescription, NdisPhysicalMedium, PhysicalMediaType | ConvertTo-Json -Compress";

    let output = match run_hidden_command("powershell", &["-NoProfile", "-Command", command]) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    if !output.status.success() {
        return HashMap::new();
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return HashMap::new();
    }

    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    let rows: Vec<Value> = if let Some(list) = parsed.as_array() {
        list.clone()
    } else {
        vec![parsed]
    };

    let mut out = HashMap::new();
    for row in rows {
        let Some(obj) = row.as_object() else {
            continue;
        };

        let Some(name) = obj.get("Name").and_then(Value::as_str) else {
            continue;
        };

        let category = categorize_interface(obj);
        out.insert(name.trim().to_string(), category);
    }

    out
}

fn load_interface_device_names() -> HashMap<String, String> {
    let command = "Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue | Select-Object Name, InterfaceDescription | ConvertTo-Json -Compress";

    let output = match run_hidden_command("powershell", &["-NoProfile", "-Command", command]) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    if !output.status.success() {
        return HashMap::new();
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return HashMap::new();
    }

    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    let rows: Vec<Value> = if let Some(list) = parsed.as_array() {
        list.clone()
    } else {
        vec![parsed]
    };

    let mut out = HashMap::new();
    for row in rows {
        let Some(obj) = row.as_object() else {
            continue;
        };

        let Some(name) = obj.get("Name").and_then(Value::as_str) else {
            continue;
        };

        let device_name = obj
            .get("InterfaceDescription")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();

        if !name.trim().is_empty() && !device_name.is_empty() {
            out.insert(name.trim().to_string(), device_name);
        }
    }

    out
}

fn categorize_interface(adapter: &serde_json::Map<String, Value>) -> u8 {
    let is_physical = adapter
        .get("HardwareInterface")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !is_physical {
        return 2;
    }

    let searchable = [
        adapter
            .get("Name")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        adapter
            .get("InterfaceDescription")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        adapter
            .get("NdisPhysicalMedium")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        adapter
            .get("PhysicalMediaType")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    ]
    .join(" ")
    .to_lowercase();

    if ["wireless", "wifi", "wi-fi", "wlan", "802.11", "native802_11"]
        .iter()
        .any(|m| searchable.contains(m))
    {
        return 1;
    }

    0
}

fn get_interface_ipv4_settings(interface: &str) -> Option<InterfaceSettings> {
    if interface.trim().is_empty() {
        return None;
    }

    let escaped = serde_json::to_string(interface).ok()?;
    let command = format!(
        "$alias = {}; $ipif = Get-NetIPInterface -InterfaceAlias $alias -AddressFamily IPv4 -ErrorAction SilentlyContinue | Select-Object -First 1; Get-NetIPConfiguration -InterfaceAlias $alias -Detailed -ErrorAction SilentlyContinue | Select-Object -First 1 -Property @(@{{Name='DhcpEnabled'; Expression={{ if ($ipif) {{ [bool]($ipif.Dhcp -eq 'Enabled') }} else {{ $null }} }}}}, @{{Name='IPAddress'; Expression={{ ($_.IPv4Address | Select-Object -First 1).IPAddress }}}}, @{{Name='PrefixLength'; Expression={{ ($_.IPv4Address | Select-Object -First 1).PrefixLength }}}}, @{{Name='DefaultGateway'; Expression={{ ($_.IPv4DefaultGateway | Select-Object -First 1).NextHop }}}}) | ConvertTo-Json -Compress",
        escaped
    );

    let output = run_hidden_command("powershell", &["-NoProfile", "-Command", &command]).ok()?;
    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }

    let data: Value = serde_json::from_str(&raw).ok()?;
    let obj = data.as_object()?;

    let ip = obj
        .get("IPAddress")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let gateway = obj
        .get("DefaultGateway")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let mask = obj
        .get("PrefixLength")
        .and_then(Value::as_u64)
        .and_then(|v| prefix_length_to_mask(v as i32))
        .unwrap_or_default();

    let dhcp_enabled = obj.get("DhcpEnabled").and_then(Value::as_bool);

    if ip.is_empty() && gateway.is_empty() && mask.is_empty() && dhcp_enabled.is_none() {
        return None;
    }

    Some(InterfaceSettings {
        ip,
        mask,
        gateway,
        dhcp_enabled,
    })
}

fn prefix_length_to_mask(prefix_length: i32) -> Option<String> {
    if !(0..=32).contains(&prefix_length) {
        return None;
    }

    let mask: u32 = if prefix_length == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_length)
    };

    Some(format!(
        "{}.{}.{}.{}",
        (mask >> 24) & 255,
        (mask >> 16) & 255,
        (mask >> 8) & 255,
        mask & 255
    ))
}

fn change_ip_address(
    interface: &str,
    ip: Option<&str>,
    mask: Option<&str>,
    gateway: Option<&str>,
) -> Result<(), String> {
    let commands: Vec<Vec<String>> = if ip.is_none() && mask.is_none() && gateway.is_none() {
        vec![
            vec![
                "netsh".to_string(),
                "interface".to_string(),
                "ipv4".to_string(),
                "set".to_string(),
                "address".to_string(),
                format!("name={}", interface),
                "source=dhcp".to_string(),
            ],
            vec![
                "netsh".to_string(),
                "interface".to_string(),
                "ipv4".to_string(),
                "set".to_string(),
                "dnsservers".to_string(),
                format!("name={}", interface),
                "source=dhcp".to_string(),
            ],
        ]
    } else {
        let (Some(ip), Some(mask), Some(gateway)) = (ip, mask, gateway) else {
            return Err("Provide all of ip, mask, and gateway, or provide none for DHCP.".to_string());
        };

        vec![vec![
            "netsh".to_string(),
            "interface".to_string(),
            "ipv4".to_string(),
            "set".to_string(),
            "address".to_string(),
            format!("name={}", interface),
            "source=static".to_string(),
            format!("address={}", ip),
            format!("mask={}", mask),
            format!("gateway={}", gateway),
            "gwmetric=1".to_string(),
        ]]
    };

    for command in commands {
        let mut cmd = Command::new(&command[0]);
        cmd.args(command.iter().skip(1))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let output = cmd
            .output()
            .map_err(|e| format!("Failed to run netsh: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let error = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                "Unknown error".to_string()
            };
            return Err(format!("Failed to change IP settings for '{}': {}", interface, error));
        }
    }

    Ok(())
}

fn is_ipv4(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() != 4 {
        return false;
    }

    for part in parts {
        if part.is_empty() || part.len() > 3 {
            return false;
        }
        let Ok(octet) = part.parse::<u8>() else {
            return false;
        };
        if octet.to_string() != part && !(octet == 0 && part == "0") {
            return false;
        }
    }

    true
}

#[cfg(target_os = "windows")]
fn ensure_windows_elevation() {
    // Release builds already embed a requireAdministrator manifest.
}

#[cfg(target_os = "windows")]
fn is_windows_admin() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

#[cfg(not(target_os = "windows"))]
fn ensure_windows_elevation() {}

#[cfg(not(target_os = "windows"))]
fn is_windows_admin() -> bool {
    false
}

fn interface_sort_key_with_categories(
    categories: &HashMap<String, u8>,
    name: &str,
) -> (u8, String) {
    if let Some(category) = categories.get(name) {
        return (*category, name.to_lowercase());
    }

    let name_l = name.to_lowercase();
    let is_wired = ["ethernet", "lan", "gbe", "gigabit"]
        .iter()
        .any(|m| name_l.contains(m));
    let is_wireless = ["wi-fi", "wifi", "wlan", "wireless", "802.11"]
        .iter()
        .any(|m| name_l.contains(m));

    if is_wired {
        return (0, name_l);
    }
    if is_wireless {
        return (1, name_l);
    }

    (2, name_l)
}