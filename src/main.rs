#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

const ICON_PNG: &[u8] = include_bytes!("../icon.png");
const ICON_ICO: &[u8] = include_bytes!("../icon.ico");

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui::{self, Color32, Frame, RichText, TextureHandle};
use eframe::{App, CreationContext, NativeOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::Shell::IsUserAnAdmin;
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IpProfile {
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

struct IpFlipRustApp {
    profiles: Vec<IpProfile>,
    interfaces: Vec<String>,
    interface_category_by_name: HashMap<String, u8>,
    selected_interface: String,
    ip_text: String,
    mask_text: String,
    gateway_text: String,
    current_settings_message: String,
    status_message: String,
    request_id: u64,
    interfaces_loading: bool,
    tx: Sender<WorkerMessage>,
    rx: Receiver<WorkerMessage>,
    profiles_path: PathBuf,
    logo_texture: Option<TextureHandle>,
}

impl IpFlipRustApp {
    fn new(cc: &CreationContext<'_>) -> Self {
        let profiles_path = app_data_dir().join("ip_profiles.json");
        let profiles = load_profiles(&profiles_path);

        let selected_interface = profiles
            .first()
            .map(|p| p.net_interface.clone())
            .unwrap_or_default();

        let logo_texture = load_logo_texture(cc);
        let (tx, rx) = mpsc::channel();

        let mut app = Self {
            profiles,
            interfaces: Vec::new(),
            interface_category_by_name: HashMap::new(),
            selected_interface,
            ip_text: String::new(),
            mask_text: String::new(),
            gateway_text: String::new(),
            current_settings_message: "Current settings will appear here.".to_string(),
            status_message: String::new(),
            request_id: 0,
            interfaces_loading: false,
            tx,
            rx,
            profiles_path,
            logo_texture,
        };

        if cfg!(target_os = "windows") && !is_windows_admin() {
            app.status_message =
                "Run the app as administrator before applying network settings.".to_string();
        }

        app.reload_interfaces_async();
        if !app.selected_interface.is_empty() {
            app.request_interface_settings(app.selected_interface.clone(), true);
        }

        app
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
        self.status_message = "Loading network interfaces...".to_string();

        let tx = self.tx.clone();
        thread::spawn(move || {
            let interfaces = list_network_interfaces();
            let categories = load_interface_categories();
            let _ = tx.send(WorkerMessage::InterfacesLoaded {
                interfaces,
                categories,
            });
        });
    }

    fn request_interface_settings(&mut self, interface_name: String, fill_form: bool) {
        if interface_name.trim().is_empty() {
            self.current_settings_message = "Current: Select an interface.".to_string();
            return;
        }

        self.request_id = self.request_id.saturating_add(1);
        let current_id = self.request_id;
        self.current_settings_message = format!("Current ({}): reading...", interface_name);

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
            self.status_message = "Interface is required.".to_string();
            return;
        }
        if ip.is_empty() || mask.is_empty() || gateway.is_empty() {
            self.status_message =
                "For static mode, interface, ip, mask, and gateway are required.".to_string();
            return;
        }
        if !is_ipv4(&ip) || !is_ipv4(&mask) || !is_ipv4(&gateway) {
            self.status_message = "IP, mask, and gateway must be valid IPv4 values.".to_string();
            return;
        }

        self.status_message = "Applying static IP...".to_string();
        let tx = self.tx.clone();
        let profile = IpProfile {
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

        self.status_message = "Applying DHCP...".to_string();
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
            self.status_message = "Interface is required.".to_string();
            return;
        }
        if ip.is_empty() || mask.is_empty() || gateway.is_empty() {
            self.status_message = "To save, interface, ip, mask, and gateway are required.".to_string();
            return;
        }
        if !is_ipv4(&ip) || !is_ipv4(&mask) || !is_ipv4(&gateway) {
            self.status_message = "IP, mask, and gateway must be valid IPv4 values.".to_string();
            return;
        }

        let profile = IpProfile {
            net_interface: interface.clone(),
            ip,
            mask,
            gateway,
        };

        self.add_profile(profile);
        self.status_message = "Configuration saved.".to_string();
    }

    fn add_profile(&mut self, profile: IpProfile) {
        if self.profiles.iter().any(|p| p == &profile) {
            return;
        }

        self.profiles.push(profile.clone());
        save_profiles(&self.profiles_path, &self.profiles);

        if !profile.net_interface.is_empty() && !self.interfaces.contains(&profile.net_interface) {
            self.interfaces.push(profile.net_interface.clone());
            let categories = self.interface_category_by_name.clone();
            self.interfaces
                .sort_by_key(|n| interface_sort_key_with_categories(&categories, n));
        }
    }

    fn process_worker_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMessage::InterfacesLoaded {
                    mut interfaces,
                    categories,
                } => {
                    self.interfaces_loading = false;
                    self.interface_category_by_name = categories;

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
                            "Current ({}, {})\nIP: {}\nMask: {}\nGateway: {}",
                            interface_name, mode, ip_text, mask_text, gateway_text
                        );
                    } else {
                        self.current_settings_message =
                            format!("Current ({}): unavailable", interface_name);
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
                        self.status_message = success_message;
                        self.request_interface_settings(interface_name, true);
                    }
                    Err(err) => {
                        self.status_message = err;
                    }
                },
            }
        }
    }
}

impl App for IpFlipRustApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_worker_messages();

        let panel_fill = Color32::from_rgb(49, 51, 71);
        let window_fill = Color32::from_rgb(32, 34, 49);
        let text_primary = Color32::from_rgb(247, 247, 242);
        let text_secondary = Color32::from_rgb(189, 194, 209);

        let mut style = (*ctx.style()).clone();
        style.visuals.window_fill = window_fill;
        style.visuals.panel_fill = window_fill;
        style.visuals.override_text_color = Some(text_primary);
        ctx.set_style(style);

        egui::CentralPanel::default()
            .frame(Frame::new().fill(window_fill).inner_margin(egui::Margin::same(16)))
            .show(ctx, |ui| {
                ui.columns(2, |columns| {
                    Frame::new()
                        .fill(panel_fill)
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(12))
                        .show(&mut columns[0], |ui| {
                            ui.label(RichText::new("Saved Profiles").strong().size(20.0));
                            ui.add_space(8.0);

                            egui::ScrollArea::vertical().show(ui, |ui| {
                                for profile in self.profiles.clone() {
                                    let text = format!(
                                        "{} | {}\nMask: {}  Gateway: {}",
                                        profile.net_interface, profile.ip, profile.mask, profile.gateway
                                    );
                                    let response = ui.add_sized(
                                        [ui.available_width(), 68.0],
                                        egui::Button::new(RichText::new(text).color(text_primary)),
                                    );

                                    if response.clicked() {
                                        self.selected_interface = profile.net_interface.clone();
                                        self.ip_text = profile.ip.clone();
                                        self.mask_text = profile.mask.clone();
                                        self.gateway_text = profile.gateway.clone();
                                        self.status_message = "Loaded saved profile into form.".to_string();
                                    }

                                    if response.double_clicked() {
                                        self.selected_interface = profile.net_interface.clone();
                                        self.ip_text = profile.ip.clone();
                                        self.mask_text = profile.mask.clone();
                                        self.gateway_text = profile.gateway.clone();
                                        self.apply_static_async();
                                    }
                                }
                            });
                        });

                    Frame::new()
                        .fill(panel_fill)
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(12))
                        .show(&mut columns[1], |ui| {
                            ui.label(RichText::new("Apply Network Settings").strong().size(20.0));
                            ui.add_space(8.0);

                            let mut selected_changed = false;
                            egui::ComboBox::from_id_salt("interface_spinner")
                                .selected_text(if self.selected_interface.is_empty() {
                                    "Select interface".to_string()
                                } else {
                                    self.selected_interface.clone()
                                })
                                .width(ui.available_width())
                                .show_ui(ui, |ui| {
                                    for interface in self.interfaces.clone() {
                                        let response = ui.selectable_value(
                                            &mut self.selected_interface,
                                            interface.clone(),
                                            interface,
                                        );
                                        if response.changed() {
                                            selected_changed = true;
                                        }
                                    }
                                });

                            if selected_changed {
                                self.request_interface_settings(self.selected_interface.clone(), true);
                            }

                            ui.add_space(8.0);
                            ui.label(RichText::new(self.current_settings_message.clone()).color(text_secondary));
                            ui.add_space(8.0);

                            if self.mask_text.is_empty() && !self.ip_text.trim().is_empty() {
                                self.mask_text = "255.255.255.0".to_string();
                            }

                            ui.add(
                                egui::TextEdit::singleline(&mut self.ip_text)
                                    .hint_text("IP")
                                    .desired_width(ui.available_width()),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut self.mask_text)
                                    .hint_text("Mask")
                                    .desired_width(ui.available_width()),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut self.gateway_text)
                                    .hint_text("Gateway")
                                    .desired_width(ui.available_width()),
                            );

                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let static_btn = egui::Button::new(
                                    RichText::new("Apply Static IP").strong().color(Color32::from_rgb(28, 31, 41)),
                                )
                                .fill(Color32::from_rgb(79, 250, 122));
                                if ui.add(static_btn).clicked() {
                                    self.apply_static_async();
                                }

                                let save_btn = egui::Button::new(
                                    RichText::new("Save profile").strong().color(Color32::from_rgb(28, 31, 41)),
                                )
                                .fill(Color32::from_rgb(140, 232, 252));
                                if ui.add(save_btn).clicked() {
                                    self.save_profile();
                                }

                                let dhcp_btn = egui::Button::new(
                                    RichText::new("Set DHCP(Automatic IP)")
                                        .strong()
                                        .color(Color32::from_rgb(28, 31, 41)),
                                )
                                .fill(Color32::from_rgb(255, 184, 107));
                                if ui.add(dhcp_btn).clicked() {
                                    self.apply_dhcp_async();
                                }
                            });

                            ui.add_space(8.0);
                            ui.label(RichText::new(self.status_message.clone()).color(text_primary));
                            ui.label(
                                RichText::new(
                                    "Note: Changing IP settings on Windows usually requires running as administrator.",
                                )
                                .color(text_secondary),
                            );

                            ui.add_space(12.0);
                            if let Some(texture) = &self.logo_texture {
                                let width = ui.available_width().min(240.0);
                                let size = texture.size_vec2();
                                let ratio = if size.x > 0.0 { size.y / size.x } else { 1.0 };
                                ui.image((texture.id(), egui::vec2(width, width * ratio)));
                            }
                        });
                });
            });

        ctx.request_repaint();
    }
}

fn main() {
    ensure_windows_elevation();

    let scale = system_scale_factor();
    let (w, h) = (1000.0 / scale, 800.0 / scale);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([w, h])
        .with_min_inner_size([w, h]);
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = NativeOptions {
        viewport,
        ..Default::default()
    };

    let _ = eframe::run_native(
        "ipFlip (Rust)",
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

fn load_logo_texture(cc: &CreationContext<'_>) -> Option<TextureHandle> {
    let image = image::load_from_memory(ICON_PNG).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &image);
    Some(cc.egui_ctx.load_texture("ipflip-logo", color_image, egui::TextureOptions::LINEAR))
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
