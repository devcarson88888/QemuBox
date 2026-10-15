use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use serde::{Deserialize, Serialize};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 720.0])
            .with_min_inner_size([780.0, 560.0])
            .with_title("QemuBox"),
        ..Default::default()
    };

    eframe::run_native(
        "QemuBox",
        options,
        Box::new(|creation_context| {
            configure_fonts(&creation_context.egui_ctx);
            Ok(Box::new(QemuBoxApp::new()))
        }),
    )
}

fn configure_fonts(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in cjk_font_paths() {
        if let Ok(data) = fs::read(path) {
            let name = "system_cjk".to_owned();
            fonts
                .font_data
                .insert(name.clone(), egui::FontData::from_owned(data).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, name.clone());
            }
            context.set_fonts(fonts);
            return;
        }
    }
    eprintln!("QemuBox: no supported CJK font found; Chinese text may not display.");
}

#[cfg(windows)]
fn cjk_font_paths() -> &'static [&'static str] {
    &[
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\Deng.ttf",
    ]
}

#[cfg(target_os = "macos")]
fn cjk_font_paths() -> &'static [&'static str] {
    &[
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
    ]
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn cjk_font_paths() -> &'static [&'static str] {
    &[
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    ]
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
enum DiskFormat {
    #[default]
    Qcow2,
    Raw,
}

impl DiskFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Qcow2 => "qcow2",
            Self::Raw => "raw",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct VmConfig {
    id: u128,
    name: String,
    qemu_path: String,
    memory_mib: u32,
    cpus: u32,
    disk_path: String,
    disk_format: DiskFormat,
    iso_path: String,
}

impl VmConfig {
    fn new() -> Self {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        Self {
            id,
            name: "新建虚拟机".to_owned(),
            qemu_path: default_qemu_path().to_owned(),
            memory_mib: 4096,
            cpus: 2,
            disk_path: String::new(),
            disk_format: DiskFormat::Qcow2,
            iso_path: String::new(),
        }
    }
}

#[cfg(windows)]
fn default_qemu_path() -> &'static str {
    "qemu-system-x86_64.exe"
}

#[cfg(not(windows))]
fn default_qemu_path() -> &'static str {
    "qemu-system-x86_64"
}

struct RunningVm {
    child: Child,
}

struct QemuBoxApp {
    config_file: PathBuf,
    vms: Vec<VmConfig>,
    selected_id: Option<u128>,
    draft: VmConfig,
    running: HashMap<u128, RunningVm>,
    notice: Option<(String, bool)>,
    page: AppPage,
    search_query: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppPage {
    Home,
    Editor,
}

impl QemuBoxApp {
    fn new() -> Self {
        let config_file = config_file_path();
        let mut notice = None;
        let vms = match load_vms(&config_file) {
            Ok(vms) => vms,
            Err(error) => {
                notice = Some((format!("读取配置失败：{error}"), true));
                Vec::new()
            }
        };
        let draft = vms.first().cloned().unwrap_or_else(VmConfig::new);
        let selected_id = vms.first().map(|vm| vm.id);

        Self {
            config_file,
            vms,
            selected_id,
            draft,
            running: HashMap::new(),
            notice,
            page: AppPage::Home,
            search_query: String::new(),
        }
    }

    fn save_all(&mut self) -> bool {
        match save_vms(&self.config_file, &self.vms) {
            Ok(()) => {
                self.notice = Some(("虚拟机配置已保存".to_owned(), false));
                true
            }
            Err(error) => {
                self.notice = Some((format!("保存配置失败：{error}"), true));
                false
            }
        }
    }

    fn save_current(&mut self) -> bool {
        if self.draft.name.trim().is_empty() {
            self.notice = Some(("虚拟机名称不能为空".to_owned(), true));
            return false;
        }

        if let Some(index) = self
            .vms
            .iter()
            .position(|vm| Some(vm.id) == self.selected_id)
        {
            self.vms[index] = self.draft.clone();
        } else {
            self.selected_id = Some(self.draft.id);
            self.vms.push(self.draft.clone());
        }
        self.save_all()
    }

    fn add_vm(&mut self) {
        self.draft = VmConfig::new();
        self.selected_id = Some(self.draft.id);
        self.page = AppPage::Editor;
    }

    fn import_vm(&mut self) {
        let Some(path) = Self::pick_file("", &["json"]) else {
            return;
        };
        let result = fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|contents| {
                serde_json::from_str::<VmConfig>(&contents).map_err(|error| error.to_string())
            });
        match result {
            Ok(mut vm) => {
                if let Some(existing) = self.vms.iter_mut().find(|item| item.id == vm.id) {
                    *existing = vm.clone();
                } else {
                    vm.id = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos();
                    self.vms.push(vm.clone());
                }
                self.selected_id = Some(vm.id);
                self.draft = vm;
                self.page = AppPage::Editor;
                self.save_all();
            }
            Err(error) => {
                self.notice = Some((format!("打开虚拟机配置失败：{error}"), true));
            }
        }
    }

    fn start_vm(&mut self, vm: &VmConfig) {
        if self.running.contains_key(&vm.id) {
            return;
        }
        if vm.disk_path.trim().is_empty() {
            self.notice = Some(("请先选择虚拟机磁盘镜像".to_owned(), true));
            return;
        }

        let mut command = Command::new(&vm.qemu_path);
        command
            .args(build_qemu_args(vm))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }

        match command.spawn() {
            Ok(child) => {
                self.running.insert(vm.id, RunningVm { child });
                self.notice = Some((format!("已启动：{}", vm.name), false));
            }
            Err(error) => {
                self.notice = Some((
                    format!("启动 QEMU 失败：{error}。请检查 QEMU 程序路径是否正确。"),
                    true,
                ));
            }
        }
    }

    fn stop_vm(&mut self, id: u128) {
        if let Some(mut running) = self.running.remove(&id) {
            match running.child.kill() {
                Ok(()) => {
                    let _ = running.child.wait();
                    self.notice = Some(("虚拟机已停止".to_owned(), false));
                }
                Err(error) => {
                    self.running.insert(id, running);
                    self.notice = Some((format!("停止虚拟机失败：{error}"), true));
                }
            }
        }
    }

    fn refresh_processes(&mut self) {
        let mut exited = Vec::new();
        for (id, running) in &mut self.running {
            match running.child.try_wait() {
                Ok(Some(_)) => exited.push(*id),
                Ok(None) => {}
                Err(error) => {
                    self.notice = Some((format!("检查虚拟机状态失败：{error}"), true));
                }
            }
        }
        for id in exited {
            self.running.remove(&id);
        }
    }

    fn pick_file(current_path: &str, extensions: &[&str]) -> Option<String> {
        let mut dialog = rfd::FileDialog::new();
        if !current_path.is_empty() {
            if let Some(parent) = Path::new(current_path).parent() {
                dialog = dialog.set_directory(parent);
            }
        }
        dialog
            .add_filter("支持的文件", extensions)
            .pick_file()
            .map(|path| path.to_string_lossy().into_owned())
    }
}

impl eframe::App for QemuBoxApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh_processes();
        context.request_repaint_after(Duration::from_secs(1));

        egui::TopBottomPanel::top("workstation_menu").show(context, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("文件(F)", |ui| {
                    if ui.button("新建虚拟机…").clicked() {
                        self.add_vm();
                        ui.close_menu();
                    }
                    if ui.button("打开虚拟机…").clicked() {
                        self.import_vm();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("退出").clicked() {
                        context.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("编辑(E)", |ui| {
                    if ui.button("虚拟机设置…").clicked() {
                        self.page = AppPage::Editor;
                        ui.close_menu();
                    }
                });
                ui.menu_button("查看(V)", |ui| {
                    if ui.button("主页").clicked() {
                        self.page = AppPage::Home;
                        ui.close_menu();
                    }
                });
                ui.menu_button("虚拟机(M)", |ui| {
                    if let Some(id) = self.selected_id {
                        if !self.running.contains_key(&id) && ui.button("启动").clicked() {
                            let vm = self.vms.iter().find(|vm| vm.id == id).cloned();
                            if let Some(vm) = vm {
                                self.start_vm(&vm);
                            }
                            ui.close_menu();
                        }
                        if self.running.contains_key(&id) && ui.button("关闭").clicked() {
                            self.stop_vm(id);
                            ui.close_menu();
                        }
                    }
                });
                ui.menu_button("帮助(H)", |ui| {
                    ui.label("QemuBox - QEMU 虚拟机管理器");
                });
            });
        });

        egui::TopBottomPanel::top("workstation_toolbar").show(context, |ui| {
            ui.horizontal(|ui| {
                if ui.button("＋ 新建").clicked() {
                    self.add_vm();
                }
                if ui.button("↗ 打开").clicked() {
                    self.import_vm();
                }
                ui.separator();
                let selected_running = self
                    .selected_id
                    .map(|id| self.running.contains_key(&id))
                    .unwrap_or(false);
                if selected_running {
                    if ui.button("■ 停止").clicked() {
                        if let Some(id) = self.selected_id {
                            self.stop_vm(id);
                        }
                    }
                } else if let Some(id) = self.selected_id {
                    if ui.button("▶ 启动").clicked() {
                        let vm = self.vms.iter().find(|vm| vm.id == id).cloned();
                        if let Some(vm) = vm {
                            self.start_vm(&vm);
                        }
                    }
                }
            });
        });

        egui::TopBottomPanel::top("workstation_tabs").show(context, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.page == AppPage::Home, "⌂ 主页")
                    .clicked()
                {
                    self.page = AppPage::Home;
                }
                if self.page == AppPage::Editor {
                    let tab_title = self
                        .vms
                        .iter()
                        .find(|vm| Some(vm.id) == self.selected_id)
                        .map(|vm| vm.name.as_str())
                        .unwrap_or(&self.draft.name)
                        .to_owned();
                    if ui
                        .selectable_label(true, format!("▣ {tab_title}"))
                        .clicked()
                    {
                        self.page = AppPage::Editor;
                    }
                }
            });
        });

        egui::SidePanel::left("vm_list")
            .resizable(true)
            .default_width(250.0)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("库");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("＋").on_hover_text("新建虚拟机").clicked() {
                            self.add_vm();
                        }
                    });
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("⌕ 搜索虚拟机")
                        .desired_width(f32::INFINITY),
                );
                ui.separator();
                ui.collapsing("我的计算机", |ui| {
                    if self.vms.is_empty() {
                        ui.label("虚拟机将显示在这里");
                    }

                    let vms = self.vms.clone();
                    for vm in vms.into_iter().filter(|vm| {
                        vm.name
                            .to_lowercase()
                            .contains(&self.search_query.to_lowercase())
                    }) {
                        ui.horizontal(|ui| {
                            let is_running = self.running.contains_key(&vm.id);
                            let selected =
                                self.selected_id == Some(vm.id) && self.page == AppPage::Editor;
                            let marker = if is_running { "●" } else { "▣" };
                            if ui
                                .selectable_label(selected, format!("{marker} {}", vm.name))
                                .clicked()
                            {
                                self.selected_id = Some(vm.id);
                                self.draft = vm.clone();
                                self.page = AppPage::Editor;
                            }
                        });
                    }
                });
            });

        egui::CentralPanel::default().show(context, |ui| {
            if self.page == AppPage::Home {
                ui.vertical_centered(|ui| {
                    ui.add_space(32.0);
                    ui.heading(egui::RichText::new("QemuBox").size(32.0).strong());
                    ui.label("Rust QEMU 虚拟机工作站");
                    ui.add_space(28.0);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 540.0).max(0.0) / 2.0);
                        if home_action_card(ui, "⊕", "创建新的虚拟机", "配置硬件并安装操作系统")
                        {
                            self.add_vm();
                        }
                        if home_action_card(ui, "↗", "打开虚拟机", "导入已有的虚拟机配置")
                        {
                            self.import_vm();
                        }
                        if home_action_card(ui, "⇄", "连接远程服务器", "远程连接暂未支持")
                        {
                            self.notice = Some(("远程 QEMU 连接功能正在开发中".to_owned(), false));
                        }
                    });
                    if let Some((message, is_error)) = &self.notice {
                        ui.add_space(24.0);
                        let color = if *is_error {
                            egui::Color32::from_rgb(240, 100, 100)
                        } else {
                            egui::Color32::from_rgb(120, 190, 145)
                        };
                        ui.colored_label(color, message);
                    }
                });
                return;
            }

            ui.heading("虚拟机配置");
            ui.label("设置 QEMU 程序、计算资源和启动介质。");
            ui.add_space(12.0);

            egui::Grid::new("vm_config")
                .num_columns(2)
                .spacing([12.0, 12.0])
                .show(ui, |ui| {
                    ui.label("名称");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.name).hint_text("例如：Ubuntu"),
                    );
                    ui.end_row();

                    ui.label("QEMU 程序");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.qemu_path)
                                .desired_width(380.0)
                                .hint_text(default_qemu_path()),
                        );
                        if ui.button("浏览…").clicked() {
                            if let Some(path) = Self::pick_file(&self.draft.qemu_path, &["exe", ""])
                            {
                                self.draft.qemu_path = path;
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("内存");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.memory_mib)
                            .range(128..=1_048_576)
                            .suffix(" MiB"),
                    );
                    ui.end_row();

                    ui.label("CPU 核心数");
                    ui.add(egui::DragValue::new(&mut self.draft.cpus).range(1..=256));
                    ui.end_row();

                    ui.label("虚拟磁盘");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.disk_path)
                                .desired_width(340.0)
                                .hint_text("选择已有磁盘镜像"),
                        );
                        if ui.button("浏览…").clicked() {
                            if let Some(path) =
                                Self::pick_file(&self.draft.disk_path, &["qcow2", "img", "raw"])
                            {
                                self.draft.disk_path = path;
                            }
                        }
                        egui::ComboBox::from_id_salt("disk_format")
                            .selected_text(self.draft.disk_format.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.draft.disk_format,
                                    DiskFormat::Qcow2,
                                    "qcow2",
                                );
                                ui.selectable_value(
                                    &mut self.draft.disk_format,
                                    DiskFormat::Raw,
                                    "raw",
                                );
                            });
                    });
                    ui.end_row();

                    ui.label("安装 ISO（可选）");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.iso_path)
                                .desired_width(380.0)
                                .hint_text("选择系统安装镜像"),
                        );
                        if ui.button("浏览…").clicked() {
                            if let Some(path) = Self::pick_file(&self.draft.iso_path, &["iso"]) {
                                self.draft.iso_path = path;
                            }
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(18.0);
            ui.horizontal(|ui| {
                if ui.button("保存配置").clicked() {
                    self.save_current();
                }
                let is_running = self
                    .selected_id
                    .map(|id| self.running.contains_key(&id))
                    .unwrap_or(false);
                if is_running {
                    if ui.button("停止虚拟机").clicked() {
                        if let Some(id) = self.selected_id {
                            self.stop_vm(id);
                        }
                    }
                } else if ui.button("保存并启动").clicked() && self.save_current() {
                    let vm = self.draft.clone();
                    self.start_vm(&vm);
                }
                if self.selected_id.is_some() && !is_running && ui.button("删除").clicked() {
                    if let Some(id) = self.selected_id {
                        self.vms.retain(|vm| vm.id != id);
                        self.selected_id = None;
                        self.draft = VmConfig::new();
                        self.save_all();
                    }
                }
            });

            if let Some((message, is_error)) = &self.notice {
                ui.add_space(10.0);
                let color = if *is_error {
                    egui::Color32::from_rgb(190, 45, 45)
                } else {
                    egui::Color32::from_rgb(45, 130, 75)
                };
                ui.colored_label(color, message);
            }

            ui.add_space(24.0);
            ui.separator();
            ui.label("提示：QEMU 需要单独安装并加入 PATH，或在上方指定其可执行文件。");
        });
    }
}

fn home_action_card(ui: &mut egui::Ui, icon: &str, title: &str, subtitle: &str) -> bool {
    let button = egui::Button::new(
        egui::RichText::new(format!("{icon}\n\n{title}\n{subtitle}"))
            .size(16.0)
            .color(egui::Color32::WHITE),
    )
    .fill(egui::Color32::from_rgb(42, 42, 42))
    .stroke(egui::Stroke::new(
        1.0_f32,
        egui::Color32::from_rgb(55, 55, 55),
    ))
    .corner_radius(egui::CornerRadius::same(6))
    .min_size(egui::vec2(164.0, 132.0));
    ui.add(button).clicked()
}

fn config_file_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("QemuBox")
        .join("vms.json")
}

fn load_vms(path: &Path) -> Result<Vec<VmConfig>, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&contents)?)
}

fn save_vms(path: &Path, vms: &[VmConfig]) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(vms)?;
    fs::write(path, contents)?;
    Ok(())
}

fn build_qemu_args(vm: &VmConfig) -> Vec<String> {
    let mut args = vec![
        "-name".to_owned(),
        vm.name.clone(),
        "-m".to_owned(),
        vm.memory_mib.to_string(),
        "-smp".to_owned(),
        vm.cpus.to_string(),
        "-drive".to_owned(),
        format!(
            "file={},format={},if=virtio",
            vm.disk_path.replace(',', "\\,"),
            vm.disk_format.label()
        ),
    ];
    if !vm.iso_path.trim().is_empty() {
        args.push("-cdrom".to_owned());
        args.push(vm.iso_path.clone());
        args.extend(["-boot".to_owned(), "order=d".to_owned()]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_qemu_arguments_with_optional_iso() {
        let vm = VmConfig {
            id: 1,
            name: "Test VM".to_owned(),
            qemu_path: "qemu-system-x86_64".to_owned(),
            memory_mib: 2048,
            cpus: 4,
            disk_path: "C:\\VMs\\disk,qcow2".to_owned(),
            disk_format: DiskFormat::Qcow2,
            iso_path: "C:\\ISOs\\linux.iso".to_owned(),
        };

        assert_eq!(
            build_qemu_args(&vm),
            [
                "-name",
                "Test VM",
                "-m",
                "2048",
                "-smp",
                "4",
                "-drive",
                "file=C:\\VMs\\disk\\,qcow2,format=qcow2,if=virtio",
                "-cdrom",
                "C:\\ISOs\\linux.iso",
                "-boot",
                "order=d",
            ]
        );
    }

    #[test]
    fn omits_iso_arguments_when_not_configured() {
        let mut vm = VmConfig::new();
        vm.disk_path = "disk.raw".to_owned();
        vm.disk_format = DiskFormat::Raw;
        assert!(!build_qemu_args(&vm).iter().any(|arg| arg == "-cdrom"));
    }
}
