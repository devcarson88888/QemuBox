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
        Box::new(|_creation_context| Ok(Box::new(QemuBoxApp::new()))),
    )
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

        egui::TopBottomPanel::top("header").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.heading("QemuBox");
                ui.label("Rust QEMU 虚拟机管理器");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("＋ 新建虚拟机").clicked() {
                        self.add_vm();
                    }
                });
            });
        });

        egui::SidePanel::left("vm_list")
            .resizable(true)
            .default_width(250.0)
            .show(context, |ui| {
                ui.heading("虚拟机");
                ui.separator();
                if self.vms.is_empty() {
                    ui.label("还没有虚拟机，点击“新建虚拟机”开始配置。");
                }

                let vms = self.vms.clone();
                for vm in vms {
                    ui.horizontal(|ui| {
                        let is_running = self.running.contains_key(&vm.id);
                        let selected = self.selected_id == Some(vm.id);
                        let marker = if is_running { "●" } else { "○" };
                        if ui
                            .selectable_label(selected, format!("{marker} {}", vm.name))
                            .clicked()
                        {
                            self.selected_id = Some(vm.id);
                            self.draft = vm.clone();
                        }
                        if is_running {
                            if ui.small_button("停止").clicked() {
                                self.stop_vm(vm.id);
                            }
                        } else if ui.small_button("启动").clicked() {
                            self.start_vm(&vm);
                        }
                    });
                }
            });

        egui::CentralPanel::default().show(context, |ui| {
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
