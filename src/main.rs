use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use serde::{Deserialize, Serialize};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 720.0])
            .with_min_inner_size([780.0, 560.0])
            .with_title("QemuBox Workstation Pro"),
        ..Default::default()
    };

    eframe::run_native(
        "QemuBox Workstation Pro",
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
                fonts.families.entry(family).or_default().push(name.clone());
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
    #[serde(default = "default_machine_type")]
    machine_type: String,
    #[serde(default = "default_acceleration")]
    acceleration: String,
    #[serde(default = "default_network_enabled")]
    network_enabled: bool,
    #[serde(default)]
    additional_qemu_args: String,
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
            machine_type: default_machine_type().to_owned(),
            acceleration: default_acceleration().to_owned(),
            network_enabled: true,
            additional_qemu_args: String::new(),
        }
    }
}

fn default_machine_type() -> String {
    "q35".to_owned()
}

fn default_acceleration() -> String {
    "tcg".to_owned()
}

fn default_network_enabled() -> bool {
    true
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
    create_vm_draft: Option<CreateVmDraft>,
    remote_config_file: PathBuf,
    remote_servers: Vec<RemoteServer>,
    remote_draft: RemoteServer,
    active_remote: Option<RemoteServer>,
    remote_vms: Vec<RemoteVm>,
    remote_busy: bool,
    remote_sender: Sender<RemoteResult>,
    remote_receiver: Receiver<RemoteResult>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppPage {
    Home,
    Editor,
    RemoteConnect,
    Remote,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CreateVmMode {
    Typical,
    Custom,
}

struct CreateVmDraft {
    vm: VmConfig,
    mode: Option<CreateVmMode>,
    disk_path: String,
    disk_size_gib: u32,
    disk_created: bool,
}

#[derive(Clone, Copy)]
enum CreateVmAction {
    Cancel,
    Back,
    Save,
    Start,
    Typical,
    Custom,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RemoteServer {
    id: u128,
    name: String,
    host: String,
    username: String,
    port: u16,
}

#[derive(Clone, Debug)]
struct RemoteVm {
    uuid: String,
    name: String,
    state: String,
}

enum RemoteCommand {
    Refresh,
    Start(String),
    Shutdown(String),
}

struct RemoteResult {
    server: RemoteServer,
    output: Result<Vec<RemoteVm>, String>,
    completion_message: String,
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
        let remote_config_file = remote_config_file_path();
        let remote_servers = match load_remote_servers(&remote_config_file) {
            Ok(servers) => servers,
            Err(error) => {
                notice = Some((format!("读取远程服务器配置失败：{error}"), true));
                Vec::new()
            }
        };
        let remote_draft = RemoteServer {
            id: 0,
            name: String::new(),
            host: String::new(),
            username: String::new(),
            port: 22,
        };
        let (remote_sender, remote_receiver) = mpsc::channel();

        Self {
            config_file,
            vms,
            selected_id,
            draft,
            running: HashMap::new(),
            notice,
            page: AppPage::Home,
            search_query: String::new(),
            create_vm_draft: None,
            remote_config_file,
            remote_servers,
            remote_draft,
            active_remote: None,
            remote_vms: Vec::new(),
            remote_busy: false,
            remote_sender,
            remote_receiver,
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
        self.create_vm_draft = Some(CreateVmDraft {
            vm: VmConfig::new(),
            mode: None,
            disk_path: String::new(),
            disk_size_gib: 40,
            disk_created: false,
        });
    }

    fn finish_create_vm(&mut self, start_after_save: bool) {
        let Some(mut draft) = self.create_vm_draft.take() else {
            return;
        };
        let vm = &mut draft.vm;
        if vm.name.trim().is_empty() {
            self.notice = Some(("虚拟机名称不能为空".to_owned(), true));
            self.create_vm_draft = Some(draft);
            return;
        }
        if draft.mode == Some(CreateVmMode::Typical) {
            vm.disk_path = draft.disk_path.trim().to_owned();
            if vm.disk_path.is_empty() || vm.iso_path.trim().is_empty() {
                self.notice = Some((
                    "典型安装需要选择 ISO 镜像和虚拟磁盘保存位置。".to_owned(),
                    true,
                ));
                self.create_vm_draft = Some(draft);
                return;
            }
            if !draft.disk_created {
                match create_virtual_disk(vm, draft.disk_size_gib) {
                    Ok(()) => draft.disk_created = true,
                    Err(error) => {
                        self.notice = Some((format!("创建虚拟磁盘失败：{error}"), true));
                        self.create_vm_draft = Some(draft);
                        return;
                    }
                }
            }
        }

        let vm = draft.vm;
        self.vms.push(vm.clone());
        if self.save_all() {
            if start_after_save {
                self.start_vm(&vm);
            }
        } else {
            self.vms.retain(|saved| saved.id != vm.id);
            draft.vm = vm;
            self.create_vm_draft = Some(draft);
        }
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

    fn connect_remote(&mut self, mut server: RemoteServer) {
        if self.remote_busy {
            return;
        }
        server.host = server.host.trim().to_owned();
        server.username = server.username.trim().to_owned();
        if !valid_ssh_server(&server) {
            self.notice = Some(("请填写有效的主机地址、用户名和 SSH 端口。".to_owned(), true));
            return;
        }
        if server.name.trim().is_empty() {
            server.name = format!("{}@{}", server.username, server.host);
        }
        if server.id == 0 {
            server.id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
        }

        self.remote_busy = true;
        self.notice = Some((format!("正在通过 SSH 连接 {}…", server.host), false));
        self.run_remote_command(server, RemoteCommand::Refresh);
    }

    fn run_remote_command(&self, server: RemoteServer, action: RemoteCommand) {
        let sender = self.remote_sender.clone();
        let completion_message = match &action {
            RemoteCommand::Refresh => "已连接并刷新".to_owned(),
            RemoteCommand::Start(_) => "启动命令已完成".to_owned(),
            RemoteCommand::Shutdown(_) => "安全关机命令已完成".to_owned(),
        };
        thread::spawn(move || {
            let output = run_remote_virsh(&server, action);
            let _ = sender.send(RemoteResult {
                server,
                output,
                completion_message,
            });
        });
    }

    fn poll_remote_results(&mut self) {
        loop {
            match self.remote_receiver.try_recv() {
                Ok(result) => {
                    self.remote_busy = false;
                    match result.output {
                        Ok(vms) => {
                            if let Some(profile) = self
                                .remote_servers
                                .iter_mut()
                                .find(|item| item.id == result.server.id)
                            {
                                *profile = result.server.clone();
                            } else {
                                self.remote_servers.push(result.server.clone());
                            }
                            self.active_remote = Some(result.server.clone());
                            self.remote_vms = vms;
                            self.page = AppPage::Remote;
                            self.notice = Some((
                                format!(
                                    "{}：{}，找到 {} 台虚拟机",
                                    result.completion_message,
                                    result.server.name,
                                    self.remote_vms.len()
                                ),
                                false,
                            ));
                            if let Err(error) =
                                save_remote_servers(&self.remote_config_file, &self.remote_servers)
                            {
                                self.notice =
                                    Some((format!("已连接，但保存服务器配置失败：{error}"), true));
                            }
                        }
                        Err(error) => {
                            self.notice = Some((format!("远程操作失败：{error}"), true));
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.remote_busy = false;
                    self.notice = Some(("远程连接工作线程已退出".to_owned(), true));
                    break;
                }
            }
        }
    }

    fn request_remote_command(&mut self, action: RemoteCommand) {
        if self.remote_busy {
            return;
        }
        if let Some(server) = self.active_remote.clone() {
            self.remote_busy = true;
            self.run_remote_command(server, action);
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
        self.poll_remote_results();
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
                    if ui.button("连接远程服务器…").clicked() {
                        self.remote_draft = RemoteServer {
                            id: 0,
                            name: String::new(),
                            host: String::new(),
                            username: String::new(),
                            port: 22,
                        };
                        self.page = AppPage::RemoteConnect;
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
                if ui.button("新建").clicked() {
                    self.add_vm();
                }
                if ui.button("打开").clicked() {
                    self.import_vm();
                }
                if ui.button("连接服务器").clicked() {
                    self.remote_draft = RemoteServer {
                        id: 0,
                        name: String::new(),
                        host: String::new(),
                        username: String::new(),
                        port: 22,
                    };
                    self.page = AppPage::RemoteConnect;
                }
                ui.separator();
                let selected_running = self
                    .selected_id
                    .map(|id| self.running.contains_key(&id))
                    .unwrap_or(false);
                if selected_running {
                    if ui.button("停止").clicked() {
                        if let Some(id) = self.selected_id {
                            self.stop_vm(id);
                        }
                    }
                } else if let Some(id) = self.selected_id {
                    if ui.button("启动").clicked() {
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
                    .selectable_label(self.page == AppPage::Home, "Home")
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
                    if ui.selectable_label(true, tab_title).clicked() {
                        self.page = AppPage::Editor;
                    }
                    if ui.small_button("x").on_hover_text("关闭标签").clicked() {
                        self.page = AppPage::Home;
                    }
                }
            });
        });

        egui::SidePanel::left("vm_list")
            .resizable(true)
            .default_width(180.0)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("My Computer");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("+").on_hover_text("新建虚拟机").clicked() {
                            self.add_vm();
                        }
                    });
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("搜索虚拟机")
                        .desired_width(f32::INFINITY),
                );
                ui.separator();
                if self.vms.is_empty() {
                    ui.label("虚拟机将显示在这里");
                }

                let vms = self.vms.clone();
                for vm in vms.into_iter().filter(|vm| {
                    vm.name
                        .to_lowercase()
                        .contains(&self.search_query.to_lowercase())
                }) {
                    let is_running = self.running.contains_key(&vm.id);
                    let selected = self.selected_id == Some(vm.id) && self.page == AppPage::Editor;
                    let marker = if is_running {
                        "[运行]"
                    } else {
                        "[虚拟机]"
                    };
                    if ui
                        .selectable_label(selected, format!("{marker} {}", vm.name))
                        .clicked()
                    {
                        self.selected_id = Some(vm.id);
                        self.draft = vm.clone();
                        self.page = AppPage::Editor;
                    }
                }
                ui.collapsing("远程服务器", |ui| {
                    if self.remote_servers.is_empty() {
                        ui.label("尚未保存服务器");
                    }
                    let servers = self.remote_servers.clone();
                    for server in servers {
                        if ui
                            .selectable_label(
                                self.active_remote.as_ref().map(|item| item.id) == Some(server.id)
                                    && self.page == AppPage::Remote,
                                server.name.clone(),
                            )
                            .clicked()
                        {
                            self.connect_remote(server);
                        }
                    }
                    if ui.button("+ 添加服务器").clicked() {
                        self.remote_draft = RemoteServer {
                            id: 0,
                            name: String::new(),
                            host: String::new(),
                            username: String::new(),
                            port: 22,
                        };
                        self.page = AppPage::RemoteConnect;
                    }
                });
            });

        egui::CentralPanel::default().show(context, |ui| {
            if self.page == AppPage::Home {
                ui.vertical(|ui| {
                    ui.add_space(54.0);
                    ui.vertical_centered(|ui| {
                        ui.heading(
                            egui::RichText::new("QemuBox WORKSTATION PRO v0.01 (Alpha)")
                                .size(29.0)
                                .strong(),
                        );
                        ui.add_space(32.0);

                        let card_gap = 18.0;
                        let available = ui.available_width();
                        let card_width = ((available - card_gap * 2.0) / 3.0).clamp(150.0, 230.0);
                        let row_width = card_width * 3.0 + card_gap * 2.0;
                        ui.horizontal(|ui| {
                            ui.add_space(((available - row_width) / 2.0).max(0.0));
                            if home_action_card(
                                ui,
                                HomeIcon::Create,
                                "创建新的虚拟机",
                                "配置硬件并安装系统",
                                card_width,
                            ) {
                                self.add_vm();
                            }
                            ui.add_space(card_gap);
                            if home_action_card(
                                ui,
                                HomeIcon::Open,
                                "打开虚拟机",
                                "导入已有的虚拟机配置",
                                card_width,
                            ) {
                                self.import_vm();
                            }
                            ui.add_space(card_gap);
                            if home_action_card(
                                ui,
                                HomeIcon::Connect,
                                "连接远程服务器",
                                "通过 SSH 管理 libvirt",
                                card_width,
                            ) {
                                self.remote_draft = RemoteServer {
                                    id: 0,
                                    name: String::new(),
                                    host: String::new(),
                                    username: String::new(),
                                    port: 22,
                                };
                                self.page = AppPage::RemoteConnect;
                            }
                        });
                        if let Some((message, is_error)) = &self.notice {
                            ui.add_space(20.0);
                            let color = if *is_error {
                                egui::Color32::from_rgb(190, 45, 45)
                            } else {
                                egui::Color32::from_rgb(45, 130, 75)
                            };
                            ui.colored_label(color, message);
                        }
                    });
                    ui.with_layout(
                        egui::Layout::bottom_up(egui::Align::LEFT),
                        |ui| {
                            ui.add_space(8.0);
                            ui.small("QemuBox Workstation Pro · Alpha");
                        },
                    );
                });
                return;
            }

            if self.page == AppPage::RemoteConnect {
                ui.heading("连接远程服务器");
                ui.label("使用 SSH 密钥连接 Linux 主机，并通过 libvirt 管理虚拟机。");
                ui.add_space(18.0);
                egui::Grid::new("remote_connection")
                    .num_columns(2)
                    .spacing([12.0, 14.0])
                    .show(ui, |ui| {
                        ui.label("连接名称");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.remote_draft.name)
                                .hint_text("可选，例如：家用服务器")
                                .desired_width(280.0),
                        );
                        ui.end_row();

                        ui.label("主机地址");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.remote_draft.host)
                                .hint_text("例如：192.168.1.20")
                                .desired_width(280.0),
                        );
                        ui.end_row();

                        ui.label("SSH 用户名");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.remote_draft.username)
                                .hint_text("Linux 用户名")
                                .desired_width(280.0),
                        );
                        ui.end_row();

                        ui.label("SSH 端口");
                        ui.add(egui::DragValue::new(&mut self.remote_draft.port).range(1..=65535));
                        ui.end_row();
                    });
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    let connect = ui.add_enabled(!self.remote_busy, egui::Button::new("连接"));
                    if connect.clicked() {
                        self.connect_remote(self.remote_draft.clone());
                    }
                    if ui.button("取消").clicked() {
                        self.page = AppPage::Home;
                    }
                });
                ui.add_space(12.0);
                ui.label("要求：本机可使用 ssh 命令；远程 Linux 主机已配置 SSH 公钥登录及 libvirt/virsh。");
                ui.label("密码不会由 QemuBox 保存或传输，请使用 SSH 密钥或 ssh-agent。");
                if let Some((message, is_error)) = &self.notice {
                    ui.add_space(8.0);
                    ui.colored_label(
                        if *is_error {
                            egui::Color32::from_rgb(240, 100, 100)
                        } else {
                            egui::Color32::from_rgb(120, 190, 145)
                        },
                        message,
                    );
                }
                return;
            }

            if self.page == AppPage::Remote {
                let server_name = self
                    .active_remote
                    .as_ref()
                    .map(|server| server.name.clone())
                    .unwrap_or_else(|| "远程服务器".to_owned());
                ui.horizontal(|ui| {
                    ui.heading(server_name);
                    if ui
                        .add_enabled(!self.remote_busy, egui::Button::new("刷新"))
                        .clicked()
                    {
                        self.request_remote_command(RemoteCommand::Refresh);
                    }
                    if self.remote_busy {
                        ui.spinner();
                        ui.label("正在执行远程操作…");
                    }
                });
                ui.label("通过 SSH 管理此主机上的 libvirt 虚拟机。");
                ui.separator();
                if self.remote_vms.is_empty() && !self.remote_busy {
                    ui.label("没有找到虚拟机，或该主机上的 libvirt 尚未配置虚拟机。");
                }
                let remote_vms = self.remote_vms.clone();
                for vm in remote_vms {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.strong(&vm.name);
                                ui.label(format!("UUID: {}", vm.uuid));
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(&vm.state);
                                    if vm.state.eq_ignore_ascii_case("running") {
                                        if ui
                                            .add_enabled(
                                                !self.remote_busy,
                                                egui::Button::new("安全关机"),
                                            )
                                            .clicked()
                                        {
                                            self.request_remote_command(RemoteCommand::Shutdown(
                                                vm.uuid.clone(),
                                            ));
                                        }
                                    } else if ui
                                        .add_enabled(
                                            !self.remote_busy,
                                            egui::Button::new("启动"),
                                        )
                                        .clicked()
                                    {
                                        self.request_remote_command(RemoteCommand::Start(
                                            vm.uuid.clone(),
                                        ));
                                    }
                                },
                            );
                        });
                    });
                }
                if let Some((message, is_error)) = &self.notice {
                    ui.add_space(8.0);
                    ui.colored_label(
                        if *is_error {
                            egui::Color32::from_rgb(240, 100, 100)
                        } else {
                            egui::Color32::from_rgb(120, 190, 145)
                        },
                        message,
                    );
                }
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

                    ui.label("虚拟机框架");
                    egui::ComboBox::from_id_salt("edit_machine_type")
                        .selected_text(&self.draft.machine_type)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.draft.machine_type, "q35".to_owned(), "q35");
                            ui.selectable_value(&mut self.draft.machine_type, "pc".to_owned(), "i440fx / pc");
                        });
                    ui.end_row();

                    ui.label("硬件加速");
                    egui::ComboBox::from_id_salt("edit_acceleration")
                        .selected_text(&self.draft.acceleration)
                        .show_ui(ui, |ui| {
                            for accelerator in ["tcg", "kvm", "whpx", "hvf"] {
                                ui.selectable_value(
                                    &mut self.draft.acceleration,
                                    accelerator.to_owned(),
                                    accelerator,
                                );
                            }
                        });
                    ui.end_row();

                    ui.label("网络");
                    ui.checkbox(&mut self.draft.network_enabled, "启用用户模式网络");
                    ui.end_row();
                });

                ui.collapsing("高级 QEMU 参数", |ui| {
                    ui.label("可填写额外参数，以空格分隔；参数会直接传给 QEMU，不经过命令行解释器。");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.draft.additional_qemu_args)
                            .desired_width(500.0)
                            .desired_rows(3)
                            .hint_text("-device virtio-vga"),
                    );
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

        if self.create_vm_draft.is_some() {
            let mut open = true;
            let mut action = None;
            let mode = self.create_vm_draft.as_ref().and_then(|draft| draft.mode);
            let title = match mode {
                None => "新建虚拟机",
                Some(CreateVmMode::Typical) => "新建虚拟机 - 典型",
                Some(CreateVmMode::Custom) => "新建虚拟机 - 自定义",
            };
            egui::Window::new(title)
                .id(egui::Id::new("create_vm_dialog"))
                .collapsible(false)
                .resizable(false)
                .default_width(if mode == Some(CreateVmMode::Custom) {
                    620.0
                } else {
                    520.0
                })
                .open(&mut open)
                .show(context, |ui| {
                    let Some(draft) = self.create_vm_draft.as_mut() else {
                        return;
                    };
                    match draft.mode {
                        None => {
                            ui.heading("选择虚拟机创建方式");
                            ui.label("请选择安装向导的引导方式。之后仍可调整硬件设置。");
                            ui.add_space(16.0);
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.set_width(230.0);
                                    ui.heading("典型");
                                    ui.label("使用推荐的 QEMU 硬件配置。选择系统 ISO，自动创建虚拟磁盘。");
                                    ui.add_space(8.0);
                                    if ui.button("典型 (推荐)").clicked() {
                                        action = Some(CreateVmAction::Typical);
                                    }
                                });
                                ui.separator();
                                ui.vertical(|ui| {
                                    ui.set_width(230.0);
                                    ui.heading("自定义");
                                    ui.label("手动选择机器类型、硬件加速、网络和额外 QEMU 参数。");
                                    ui.add_space(8.0);
                                    if ui.button("自定义 (高级)").clicked() {
                                        action = Some(CreateVmAction::Custom);
                                    }
                                });
                            });
                        }
                        Some(CreateVmMode::Typical) => {
                            ui.label("选择安装介质并为虚拟机创建一个新的 qcow2 磁盘。");
                            ui.add_space(12.0);
                            egui::Grid::new("typical_vm_config")
                                .num_columns(2)
                                .spacing([12.0, 14.0])
                                .show(ui, |ui| {
                                    ui.label("虚拟机名称");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut draft.vm.name)
                                            .desired_width(330.0)
                                            .hint_text("例如：Ubuntu"),
                                    );
                                    ui.end_row();

                                    ui.label("安装 ISO");
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut draft.vm.iso_path)
                                                .desired_width(235.0)
                                                .hint_text("选择操作系统镜像"),
                                        );
                                        if ui.button("浏览…").clicked() {
                                            if let Some(path) =
                                                Self::pick_file(&draft.vm.iso_path, &["iso"])
                                            {
                                                draft.vm.iso_path = path;
                                                if draft.vm.name == "新建虚拟机" {
                                                    if let Some(stem) =
                                                        Path::new(&draft.vm.iso_path).file_stem()
                                                    {
                                                        draft.vm.name =
                                                            stem.to_string_lossy().into_owned();
                                                    }
                                                }
                                            }
                                        }
                                    });
                                    ui.end_row();

                                    ui.label("虚拟磁盘");
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut draft.disk_path)
                                                .desired_width(235.0)
                                                .hint_text("选择保存位置"),
                                        );
                                        if ui.button("浏览…").clicked() {
                                            let default_file = format!(
                                                "{}.qcow2",
                                                draft.vm.name.replace(
                                                    ['<', '>', ':', '"', '/', '\\', '|', '?', '*'],
                                                    "_"
                                                )
                                            );
                                            if let Some(path) = rfd::FileDialog::new()
                                                .add_filter("QEMU qcow2 磁盘", &["qcow2"])
                                                .set_file_name(default_file)
                                                .save_file()
                                            {
                                                draft.disk_path =
                                                    path.to_string_lossy().into_owned();
                                            }
                                        }
                                    });
                                    ui.end_row();

                                    ui.label("磁盘容量");
                                    ui.add(
                                        egui::DragValue::new(&mut draft.disk_size_gib)
                                            .range(1..=2048)
                                            .suffix(" GiB"),
                                    );
                                    ui.end_row();
                                });
                            ui.add_space(10.0);
                            ui.label("推荐配置：q35、TCG 软件加速、virtio 磁盘和用户模式网络。");
                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                if ui.button("上一步").clicked() {
                                    action = Some(CreateVmAction::Back);
                                }
                                if ui.button("取消").clicked() {
                                    action = Some(CreateVmAction::Cancel);
                                }
                                if ui.button("创建虚拟机").clicked() {
                                    action = Some(CreateVmAction::Save);
                                }
                                if ui.button("创建并启动").clicked() {
                                    action = Some(CreateVmAction::Start);
                                }
                            });
                        }
                        Some(CreateVmMode::Custom) => {
                            ui.label("选择 QEMU 机器框架、加速器和虚拟设备，也可以添加原生 QEMU 参数。");
                            ui.add_space(10.0);
                            egui::Grid::new("custom_vm_config")
                                .num_columns(2)
                                .spacing([12.0, 10.0])
                                .show(ui, |ui| {
                                    ui.label("名称");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut draft.vm.name)
                                            .desired_width(380.0)
                                            .hint_text("例如：开发测试机"),
                                    );
                                    ui.end_row();

                                    ui.label("QEMU 程序");
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut draft.vm.qemu_path)
                                                .desired_width(290.0)
                                                .hint_text(default_qemu_path()),
                                        );
                                        if ui.button("浏览…").clicked() {
                                            if let Some(path) =
                                                Self::pick_file(&draft.vm.qemu_path, &["exe", ""])
                                            {
                                                draft.vm.qemu_path = path;
                                            }
                                        }
                                    });
                                    ui.end_row();

                                    ui.label("内存");
                                    ui.add(
                                        egui::DragValue::new(&mut draft.vm.memory_mib)
                                            .range(128..=1_048_576)
                                            .suffix(" MiB"),
                                    );
                                    ui.end_row();

                                    ui.label("CPU 核心数");
                                    ui.add(
                                        egui::DragValue::new(&mut draft.vm.cpus).range(1..=256),
                                    );
                                    ui.end_row();

                                    ui.label("机器框架");
                                    egui::ComboBox::from_id_salt("wizard_machine_type")
                                        .selected_text(&draft.vm.machine_type)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut draft.vm.machine_type,
                                                "q35".to_owned(),
                                                "q35 (PCIe)",
                                            );
                                            ui.selectable_value(
                                                &mut draft.vm.machine_type,
                                                "pc".to_owned(),
                                                "i440fx / pc",
                                            );
                                        });
                                    ui.end_row();

                                    ui.label("硬件加速");
                                    egui::ComboBox::from_id_salt("wizard_acceleration")
                                        .selected_text(&draft.vm.acceleration)
                                        .show_ui(ui, |ui| {
                                            for accelerator in ["tcg", "kvm", "whpx", "hvf"] {
                                                ui.selectable_value(
                                                    &mut draft.vm.acceleration,
                                                    accelerator.to_owned(),
                                                    accelerator,
                                                );
                                            }
                                        });
                                    ui.end_row();

                                    ui.label("虚拟磁盘");
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut draft.vm.disk_path)
                                                .desired_width(290.0)
                                                .hint_text("选择已有镜像"),
                                        );
                                        if ui.button("浏览…").clicked() {
                                            if let Some(path) = Self::pick_file(
                                                &draft.vm.disk_path,
                                                &["qcow2", "img", "raw"],
                                            ) {
                                                draft.vm.disk_path = path;
                                            }
                                        }
                                        egui::ComboBox::from_id_salt("wizard_disk_format")
                                            .selected_text(draft.vm.disk_format.label())
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut draft.vm.disk_format,
                                                    DiskFormat::Qcow2,
                                                    "qcow2",
                                                );
                                                ui.selectable_value(
                                                    &mut draft.vm.disk_format,
                                                    DiskFormat::Raw,
                                                    "raw",
                                                );
                                            });
                                    });
                                    ui.end_row();

                                    ui.label("安装 ISO");
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut draft.vm.iso_path)
                                                .desired_width(290.0)
                                                .hint_text("可选"),
                                        );
                                        if ui.button("浏览…").clicked() {
                                            if let Some(path) =
                                                Self::pick_file(&draft.vm.iso_path, &["iso"])
                                            {
                                                draft.vm.iso_path = path;
                                            }
                                        }
                                    });
                                    ui.end_row();

                                    ui.label("网络设备");
                                    ui.checkbox(
                                        &mut draft.vm.network_enabled,
                                        "用户模式网络 (NAT)",
                                    );
                                    ui.end_row();
                                });
                            ui.collapsing("额外 QEMU 参数", |ui| {
                                ui.label("直接追加到 QEMU 命令；以空格分隔，不支持带空格的单个参数值。");
                                ui.add(
                                    egui::TextEdit::multiline(
                                        &mut draft.vm.additional_qemu_args,
                                    )
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(3)
                                    .hint_text("-device virtio-vga"),
                                );
                            });
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                if ui.button("上一步").clicked() {
                                    action = Some(CreateVmAction::Back);
                                }
                                if ui.button("取消").clicked() {
                                    action = Some(CreateVmAction::Cancel);
                                }
                                if ui.button("完成").clicked() {
                                    action = Some(CreateVmAction::Save);
                                }
                                if ui.button("完成并启动").clicked() {
                                    action = Some(CreateVmAction::Start);
                                }
                            });
                        }
                    }
                });
            match action {
                Some(CreateVmAction::Cancel) => self.create_vm_draft = None,
                Some(CreateVmAction::Back) => {
                    if let Some(draft) = self.create_vm_draft.as_mut() {
                        draft.mode = None;
                    }
                }
                Some(CreateVmAction::Typical) => {
                    if let Some(draft) = self.create_vm_draft.as_mut() {
                        draft.mode = Some(CreateVmMode::Typical);
                    }
                }
                Some(CreateVmAction::Custom) => {
                    if let Some(draft) = self.create_vm_draft.as_mut() {
                        draft.mode = Some(CreateVmMode::Custom);
                    }
                }
                Some(CreateVmAction::Save) => self.finish_create_vm(false),
                Some(CreateVmAction::Start) => self.finish_create_vm(true),
                None if !open => self.create_vm_draft = None,
                None => {}
            }
        }
    }
}

#[derive(Clone, Copy)]
enum HomeIcon {
    Create,
    Open,
    Connect,
}

fn home_action_card(
    ui: &mut egui::Ui,
    icon: HomeIcon,
    title: &str,
    subtitle: &str,
    width: f32,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 176.0), egui::Sense::click());
    let fill = if response.hovered() {
        egui::Color32::from_rgb(58, 58, 58)
    } else {
        egui::Color32::from_rgb(38, 38, 38)
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::ZERO, fill);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(2.0_f32, egui::Color32::BLACK),
        egui::StrokeKind::Inside,
    );
    let center = rect.center();
    let white = egui::Color32::WHITE;
    let painter = ui.painter();
    let icon_center = egui::pos2(center.x, rect.top() + 50.0);
    let icon_stroke = egui::Stroke::new(3.0_f32, white);
    match icon {
        HomeIcon::Create => {
            painter.circle_stroke(icon_center, 21.0, icon_stroke);
            painter.line_segment(
                [
                    egui::pos2(icon_center.x - 11.0, icon_center.y),
                    egui::pos2(icon_center.x + 11.0, icon_center.y),
                ],
                icon_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(icon_center.x, icon_center.y - 11.0),
                    egui::pos2(icon_center.x, icon_center.y + 11.0),
                ],
                icon_stroke,
            );
        }
        HomeIcon::Open => {
            painter.rect_stroke(
                egui::Rect::from_center_size(
                    egui::pos2(icon_center.x - 7.0, icon_center.y + 6.0),
                    egui::vec2(38.0, 38.0),
                ),
                egui::CornerRadius::ZERO,
                icon_stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    egui::pos2(icon_center.x - 1.0, icon_center.y + 8.0),
                    egui::pos2(icon_center.x + 18.0, icon_center.y - 12.0),
                ],
                icon_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(icon_center.x + 5.0, icon_center.y - 12.0),
                    egui::pos2(icon_center.x + 18.0, icon_center.y - 12.0),
                ],
                icon_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(icon_center.x + 18.0, icon_center.y - 12.0),
                    egui::pos2(icon_center.x + 18.0, icon_center.y + 1.0),
                ],
                icon_stroke,
            );
        }
        HomeIcon::Connect => {
            let left = icon_center.x - 22.0;
            let right = icon_center.x + 22.0;
            painter.line_segment(
                [
                    egui::pos2(left, icon_center.y - 8.0),
                    egui::pos2(right, icon_center.y - 8.0),
                ],
                icon_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(right - 8.0, icon_center.y - 16.0),
                    egui::pos2(right, icon_center.y - 8.0),
                ],
                icon_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(right, icon_center.y - 8.0),
                    egui::pos2(right - 8.0, icon_center.y),
                ],
                icon_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(right, icon_center.y + 8.0),
                    egui::pos2(left, icon_center.y + 8.0),
                ],
                icon_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(left + 8.0, icon_center.y),
                    egui::pos2(left, icon_center.y + 8.0),
                ],
                icon_stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(left, icon_center.y + 8.0),
                    egui::pos2(left + 8.0, icon_center.y + 16.0),
                ],
                icon_stroke,
            );
        }
    }
    ui.painter().text(
        egui::pos2(center.x, rect.top() + 112.0),
        egui::Align2::CENTER_CENTER,
        title,
        egui::FontId::proportional(17.0),
        white,
    );
    ui.painter().text(
        egui::pos2(center.x, rect.top() + 143.0),
        egui::Align2::CENTER_CENTER,
        subtitle,
        egui::FontId::proportional(12.0),
        egui::Color32::from_gray(205),
    );
    response.clicked()
}

fn config_file_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("QemuBox")
        .join("vms.json")
}

fn remote_config_file_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("QemuBox")
        .join("remote-servers.json")
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

fn load_remote_servers(path: &Path) -> Result<Vec<RemoteServer>, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&contents)?)
}

fn save_remote_servers(
    path: &Path,
    servers: &[RemoteServer],
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(servers)?;
    fs::write(path, contents)?;
    Ok(())
}

fn valid_ssh_server(server: &RemoteServer) -> bool {
    !server.host.is_empty()
        && server
            .host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
        && !server.username.is_empty()
        && server
            .username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && server.port > 0
}

fn valid_domain_uuid(uuid: &str) -> bool {
    uuid.len() == 36
        && uuid.chars().enumerate().all(|(index, character)| {
            if [8, 13, 18, 23].contains(&index) {
                character == '-'
            } else {
                character.is_ascii_hexdigit()
            }
        })
}

fn run_ssh(server: &RemoteServer, remote_command: &str) -> Result<Output, String> {
    if !valid_ssh_server(server) {
        return Err("服务器地址或用户名无效".to_owned());
    }
    let target = format!("{}@{}", server.username, server.host);
    let output = Command::new("ssh")
        .args([
            "-p",
            &server.port.to_string(),
            "-oBatchMode=yes",
            "-oConnectTimeout=10",
            "-oServerAliveInterval=5",
            "-oServerAliveCountMax=2",
            &target,
            remote_command,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "找不到 ssh 命令，请安装 OpenSSH 客户端并将 ssh 加入 PATH。".to_owned()
            } else {
                format!("无法启动 SSH：{error}")
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let message = if stderr.is_empty() {
            format!("SSH 命令退出，状态码：{}", output.status)
        } else {
            stderr
        };
        return Err(message);
    }
    Ok(output)
}

fn run_remote_virsh(server: &RemoteServer, action: RemoteCommand) -> Result<Vec<RemoteVm>, String> {
    let action_command = match action {
        RemoteCommand::Refresh => None,
        RemoteCommand::Start(uuid) => {
            if !valid_domain_uuid(&uuid) {
                return Err("虚拟机 UUID 无效".to_owned());
            }
            Some(format!("virsh start {uuid}"))
        }
        RemoteCommand::Shutdown(uuid) => {
            if !valid_domain_uuid(&uuid) {
                return Err("虚拟机 UUID 无效".to_owned());
            }
            Some(format!("virsh shutdown {uuid}"))
        }
    };

    if let Some(command) = action_command {
        run_ssh(server, &command)?;
    }

    let output = run_ssh(server, "virsh list --all --uuid")?;
    let uuids = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|uuid| !uuid.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    let mut vms = Vec::with_capacity(uuids.len());
    for uuid in uuids {
        if !valid_domain_uuid(&uuid) {
            return Err("远程 virsh 返回了无效的虚拟机 UUID".to_owned());
        }
        let output = run_ssh(server, &format!("virsh dominfo {uuid}"))?;
        let info = String::from_utf8_lossy(&output.stdout);
        let mut name = None;
        let mut state = None;
        for line in info.lines() {
            if let Some((key, value)) = line.split_once(':') {
                match key.trim() {
                    "Name" => name = Some(value.trim().to_owned()),
                    "State" => state = Some(value.trim().to_owned()),
                    _ => {}
                }
            }
        }
        vms.push(RemoteVm {
            uuid,
            name: name.ok_or_else(|| "virsh dominfo 未返回虚拟机名称".to_owned())?,
            state: state.ok_or_else(|| "virsh dominfo 未返回虚拟机状态".to_owned())?,
        });
    }
    Ok(vms)
}

fn build_qemu_args(vm: &VmConfig) -> Vec<String> {
    let mut args = vec![
        "-machine".to_owned(),
        format!(
            "type={},accel={}",
            vm.machine_type.trim(),
            vm.acceleration.trim()
        ),
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
    if vm.network_enabled {
        args.extend([
            "-netdev".to_owned(),
            "user,id=net0".to_owned(),
            "-device".to_owned(),
            "virtio-net-pci,netdev=net0".to_owned(),
        ]);
    }
    args.extend(
        vm.additional_qemu_args
            .split_whitespace()
            .map(str::to_owned),
    );
    args
}

fn create_virtual_disk(vm: &VmConfig, size_gib: u32) -> Result<(), String> {
    let path = Path::new(&vm.disk_path);
    if path.exists() {
        return Err(format!("目标文件已存在：{}", path.display()));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建目录：{error}"))?;
    }

    let output = Command::new(default_qemu_img_path())
        .args(["create", "-f", "qcow2"])
        .arg(path)
        .arg(format!("{size_gib}G"))
        .output()
        .map_err(|error| format!("无法运行 qemu-img：{error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "qemu-img 退出码 {:?}：{}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn default_qemu_img_path() -> &'static str {
    "qemu-img.exe"
}

#[cfg(not(windows))]
fn default_qemu_img_path() -> &'static str {
    "qemu-img"
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
            machine_type: "q35".to_owned(),
            acceleration: "tcg".to_owned(),
            network_enabled: true,
            additional_qemu_args: String::new(),
        };

        assert_eq!(
            build_qemu_args(&vm),
            [
                "-machine",
                "type=q35,accel=tcg",
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
                "-netdev",
                "user,id=net0",
                "-device",
                "virtio-net-pci,netdev=net0",
            ]
        );
    }

    #[test]
    fn omits_iso_arguments_when_not_configured() {
        let mut vm = VmConfig::new();
        vm.disk_path = "disk.raw".to_owned();
        vm.disk_format = DiskFormat::Raw;
        assert!(!build_qemu_args(&vm).iter().any(|arg| arg == "-cdrom"));
        assert!(build_qemu_args(&vm).iter().any(|arg| arg == "-machine"));
    }

    #[test]
    fn adds_custom_qemu_arguments_and_can_disable_network() {
        let mut vm = VmConfig::new();
        vm.additional_qemu_args = "-device virtio-vga -display none".to_owned();
        vm.network_enabled = false;
        let args = build_qemu_args(&vm);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-device", "virtio-vga"]));
        assert!(args.iter().any(|arg| arg == "-display"));
        assert!(!args.iter().any(|arg| arg == "-netdev"));
    }

    #[test]
    fn loads_legacy_vm_config_with_default_hardware_options() {
        let vm: VmConfig = serde_json::from_str(
            r#"{
                "id": 1,
                "name": "Legacy VM",
                "qemu_path": "qemu-system-x86_64",
                "memory_mib": 1024,
                "cpus": 1,
                "disk_path": "disk.qcow2",
                "disk_format": "Qcow2",
                "iso_path": ""
            }"#,
        )
        .unwrap();

        assert_eq!(vm.machine_type, "q35");
        assert_eq!(vm.acceleration, "tcg");
        assert!(vm.network_enabled);
        assert!(vm.additional_qemu_args.is_empty());
    }

    #[test]
    fn accepts_only_well_formed_domain_uuids() {
        assert!(valid_domain_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!valid_domain_uuid("virsh; rm -rf /"));
        assert!(!valid_domain_uuid("550e8400-e29b-41d4-a716-44665544000z"));
    }

    #[test]
    fn rejects_shell_metacharacters_in_remote_connection_fields() {
        let mut server = RemoteServer {
            id: 1,
            name: "test".to_owned(),
            host: "example.com".to_owned(),
            username: "user.name".to_owned(),
            port: 22,
        };
        assert!(valid_ssh_server(&server));
        server.host = "example.com;whoami".to_owned();
        assert!(!valid_ssh_server(&server));
        server.host = "example.com".to_owned();
        server.username = "user;whoami".to_owned();
        assert!(!valid_ssh_server(&server));
    }
}
