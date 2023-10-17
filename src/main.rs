#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

// import for MessageBox

use eframe::egui;
// reqwest
use reqwest;
use serde::{Deserialize, Serialize};
use shlex::Shlex;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc;
use zip;

// Alternative emoji list:
// ❗

// static variables
static DR_DOWNLOAD_URL: &str = "https://github.com/DynamoRIO/dynamorio/releases/download/release_9.0.1/DynamoRIO-Windows-9.0.1.zip";
static DR_TOOL_DOWNLOAD_URL: &str =
    "https://github.com/expend20/DrSymLogger/releases/download/v0.0.1/DrSymLogger.dll";

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
enum DrToolInstrumentationMode {
    Exec,
    Inst,
    Invalid,
}

impl Default for DrToolInstrumentationMode {
    fn default() -> Self {
        DrToolInstrumentationMode::Invalid
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Settings {
    dr_dir: String,
    dr_tool_path: String,
    inst_module: String,
    inst_mode: DrToolInstrumentationMode,
    substr: String,
    redirect_to_file: String, // 2>&1 > file.txt
    cmd: String,
}

impl Settings {
    fn default() -> Self {
        Self {
            dr_dir: "".to_owned(),
            dr_tool_path: "".to_owned(),
            inst_module: "cmd.exe".to_owned(),
            inst_mode: DrToolInstrumentationMode::Exec,
            substr: "".to_owned(),
            redirect_to_file: "log.txt".to_owned(),
            cmd: "cmd.exe /c cmd.bat".to_owned(),
        }
    }
    fn new() -> Self {
        // try to read settings.json
        let settings_path = Path::new("settings.json");
        if settings_path.exists() {
            // read settings
            let settings_str = std::fs::read_to_string(settings_path).unwrap();
            let settings: Settings = serde_json::from_str(&settings_str).unwrap();
            return settings;
        }
        Self::default()
    }
    fn save(&self) {
        let settings_str = serde_json::to_string(self).unwrap();
        std::fs::write("settings.json", settings_str).unwrap();
    }
}

fn log(msg: &str) {
    // open file "log.txt" in append mode
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("log.txt")
        .unwrap();
    // write msg to file
    file.write_all(msg.as_bytes()).unwrap();
}

fn main() -> Result<(), eframe::Error> {
    log("main started\n");
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).
    let options = eframe::NativeOptions {
        initial_window_size: Some(egui::vec2(800.0, 600.0)),
        // disable resizing
        decorated: false,
        transparent: true,
        ..Default::default()
    };
    eframe::run_native(
        "My egui App",
        options,
        Box::new(|_cc| Box::new(MyApp::new())),
    )
}

struct MyApp {
    settings: Settings,
    settings_cached: Settings,
    is_dr_dir_ok: bool,
    is_dr_tool_path_ok: bool,
    is_dr_download_started: bool,
    is_dr_tool_download_started: bool,
    is_quote_in_cmd: bool,
    log_text: String,
    on_done_dr_down_tx: mpsc::SyncSender<Option<String>>,
    on_done_tool_down_tx: mpsc::SyncSender<Option<String>>,
    on_done_dr_down_rc: mpsc::Receiver<Option<String>>,
    on_done_tool_down_rc: mpsc::Receiver<Option<String>>,
    symbol_path: String,
    cmd: String,
}

impl MyApp {
    fn new() -> Self {
        // create channel for communication between threads
        let (on_done_tx, on_done_rc) = mpsc::sync_channel::<Option<String>>(0);
        let (on_tool_done_tx, on_tool_done_rc) = mpsc::sync_channel::<Option<String>>(0);
        let settings = Settings::new();
        let settings_cached = Settings::default();
        let mut s = Self {
            is_dr_dir_ok: false,
            is_dr_tool_path_ok: false,
            is_dr_download_started: false,
            is_dr_tool_download_started: false,
            is_quote_in_cmd: false,
            log_text: "".to_owned(),
            on_done_dr_down_tx: on_done_tx,
            on_done_tool_down_tx: on_tool_done_tx,
            on_done_dr_down_rc: on_done_rc,
            on_done_tool_down_rc: on_tool_done_rc,
            symbol_path: "".to_owned(),
            settings,
            settings_cached,
            cmd: "".to_owned(),
        };
        s.check_symbol_path();
        s
    }

    fn check_symbol_path(&mut self) {
        // get env variable "_NT_SYMBOL_PATH"
        let symbol_path = std::env::var("_NT_SYMBOL_PATH").unwrap();
        if symbol_path.is_empty() {
            self.log_text.push_str(
                format!(
                    "Symbol path is empty, set it to something like \"{}\"\n",
                    "srv*c:\\symbols*https://msdl.microsoft.com/download/symbols"
                )
                .as_str(),
            );
        } else {
            self.log_text
                .push_str(&format!("_NT_SYMBOL_PATH is ok: {:?}\n", symbol_path));
            // split by * and ignore "srv" and "http" parts
            let paths: Vec<&str> = symbol_path
                .split('*')
                .filter(|x| !x.starts_with("srv") && !x.starts_with("http"))
                .collect();
            // leave only first one, and warning if there are more
            if paths.len() > 1 {
                self.log_text.push_str(
                    format!(
                        "Warning: more than one path in _NT_SYMBOL_PATH, leaving only first one: {}\n",
                        paths[0]
                    ).as_str(),
                );
                self.symbol_path = paths[0].to_owned();
            } else if paths.len() == 1 {
                self.symbol_path = paths[0].to_owned();
                self.log_text
                    .push_str(format!("Symbol path for DR: {}\n", self.symbol_path).as_str());
            } else {
                self.log_text
                    .push_str("Warning: no valid paths in _NT_SYMBOL_PATH found\n");
            }
        }
    }

    fn validate_fields_and_update_cmd(&mut self) {
        let mut is_update = false;
        if self.settings.dr_dir != self.settings_cached.dr_dir {
            self.is_dr_dir_ok = check_dr_dir(&self.settings.dr_dir);
            self.settings_cached.dr_dir = self.settings.dr_dir.clone();
            if self.is_dr_dir_ok {
                self.log_text
                    .push_str(&format!("DR dir ok: {:?}\n", self.settings.dr_dir));
                is_update = true;
            } else {
                self.log_text
                    .push_str(&format!("Invalid DR dir: {:?}\n", self.settings.dr_dir));
            }
        }

        if self.settings.dr_tool_path != self.settings_cached.dr_tool_path {
            self.is_dr_tool_path_ok = check_dr_tool_path(&self.settings.dr_tool_path);
            self.settings_cached.dr_tool_path = self.settings.dr_tool_path.clone();
            if self.is_dr_tool_path_ok {
                self.log_text.push_str(&format!(
                    "DR tool path ok: {:?}\n",
                    self.settings.dr_tool_path
                ));
                is_update = true;
            } else {
                self.log_text.push_str(&format!(
                    "Invalid DR tool path: {:?}\n",
                    self.settings.dr_tool_path
                ));
            }
        }

        if self.settings.inst_module != self.settings_cached.inst_module {
            self.settings_cached.inst_module = self.settings.inst_module.clone();
            self.log_text.push_str(&format!(
                "Instrumentation module changed: {:?}\n",
                self.settings.inst_module
            ));
            is_update = true;
        }

        if self.settings.inst_mode != self.settings_cached.inst_mode {
            self.settings_cached.inst_mode = self.settings.inst_mode.clone();
            self.log_text.push_str(&format!(
                "Instrumentation mode changed: {:?}\n",
                self.settings.inst_mode
            ));
            is_update = true;
        }

        if self.settings.redirect_to_file != self.settings_cached.redirect_to_file {
            self.settings_cached.redirect_to_file = self.settings.redirect_to_file.clone();
            self.log_text.push_str(&format!(
                "Redirect to file changed: {:?}\n",
                self.settings.redirect_to_file
            ));
            is_update = true;
        }

        if self.settings.substr != self.settings_cached.substr {
            self.settings_cached.substr = self.settings.substr.clone();
            self.log_text
                .push_str(&format!("Substring changed: {:?}\n", self.settings.substr));
            is_update = true;
        }

        if self.settings.cmd != self.settings_cached.cmd {
            self.settings_cached.cmd = self.settings.cmd.clone();
            self.log_text.push_str(&format!(
                "Target command line changed: {}\n",
                self.settings.cmd
            ));
            if self.settings.cmd.contains('"') {
                self.is_quote_in_cmd = true;
                self.log_text.push_str("Error: quote in command line\n");
            } else {
                self.is_quote_in_cmd = false;
            }
            is_update = true;
        }

        if is_update {
            // --printSymsExec vs --printSymsInst
            let str_mode = match self.settings.inst_mode {
                DrToolInstrumentationMode::Exec => "--printSymsExec",
                DrToolInstrumentationMode::Inst => "--printSymsInst",
                DrToolInstrumentationMode::Invalid => todo!(),
            };

            self.cmd = format!(
                "set _NT_SYMBOL_PATH={} && {}\\bin64\\drrun.exe -c {} {} --printSymsExecConsole --printSymsModule {}",
                self.symbol_path,
                self.settings.dr_dir,
                self.settings.dr_tool_path,
                str_mode,
                self.settings.inst_module);

            if self.settings.substr.len() > 0 {
                self.cmd
                    .push_str(&format!(" --printSymsGrep {}", self.settings.substr));
            }

            self.cmd.push_str(&format!(" -- {}", self.settings.cmd));

            if !self.settings.redirect_to_file.is_empty() {
                self.cmd
                    .push_str(format!(" > {} 2>&1", self.settings.redirect_to_file).as_str());
            }

            self.settings.save();
        }
    }

    fn show_dr_dir_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label("DinamoRIO dir");
        });
        ui.horizontal(|ui| {
            ui.set_width(ui.available_width());
            let dir_input = egui::TextEdit::singleline(&mut self.settings.dr_dir);
            ui.add_enabled(!self.is_dr_download_started, dir_input);

            let open_dir = egui::Button::new("📁🔍");
            if ui
                .add_enabled(!self.is_dr_download_started, open_dir)
                .on_hover_text("Open directory dialog")
                .clicked()
            {
                // get current directory
                let current_dir = std::env::current_dir().unwrap();
                // open file dialog
                let fd = rfd::FileDialog::new().set_directory(&current_dir);
                if let Some(result) = fd.pick_folder() {
                    self.settings.dr_dir = result.display().to_string();
                } else {
                    self.log_text.push_str("No valid directory selected\n");
                }
            }
            let down_button = egui::Button::new("🌐⬇");
            if ui
                .add_enabled(!self.is_dr_download_started, down_button)
                .on_hover_text(format!(
                    "Download from the internet to specified directory.\nUrl: {}",
                    DR_DOWNLOAD_URL
                ))
                .clicked()
            {
                // check if there is already a valid DR path
                if self.is_dr_dir_ok {
                    self.log_text.push_str("DR dir is already valid\n");
                    return;
                }

                // if input directory is a valid directory use it as a destination directory
                // otherwise pick a directory, if that is failed, use use current directory
                let dest_dir: String;
                // check if directory is valid, othervise open file dialog
                if !Path::new(&self.settings.dr_dir).exists() {
                    // get current directory
                    let current_dir = std::env::current_dir().unwrap();
                    // open file dialog
                    let fd = rfd::FileDialog::new().set_directory(&current_dir);
                    if let Some(result) = fd.pick_folder() {
                        dest_dir = result.display().to_string();
                    } else {
                        dest_dir = current_dir.display().to_string();
                    }
                } else {
                    dest_dir = self.settings.dr_dir.clone();
                }

                self.is_dr_download_started = true;
                let tx = self.on_done_dr_down_tx.clone();
                let ctx2 = ctx.clone();
                self.log_text.push_str(
                    format!("Download started {} -> {} ...\n", DR_DOWNLOAD_URL, dest_dir).as_str(),
                );
                std::thread::spawn(move || {
                    let mut resp = reqwest::blocking::get(DR_DOWNLOAD_URL).unwrap();
                    assert!(resp.status().is_success());
                    let last_part = DR_DOWNLOAD_URL.split('/').last().unwrap();
                    let last_part_no_zip = last_part.split(".zip").next().unwrap();
                    let mut out = std::fs::File::create(last_part).unwrap();
                    resp.copy_to(&mut out).unwrap();
                    extract_zip_to_dir(last_part, dest_dir.as_str());
                    ctx2.request_repaint();
                    let dest_dir = Path::new(&dest_dir).join(last_part_no_zip);
                    let _ = tx.send(Some(dest_dir.display().to_string().to_owned()));
                });
            }
            if self.is_dr_download_started {
                ui.spinner();
            }
            if !self.is_dr_dir_ok {
                ui.colored_label(egui::Color32::RED, "☹")
                    .on_hover_text("Invalid directory");
            }
        });

        ui.end_row();
    }

    fn show_tool_path_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label("DinamoRIO tool path");
        });
        ui.horizontal(|ui| {
            let dir_input = egui::TextEdit::singleline(&mut self.settings.dr_tool_path);
            ui.add_enabled(!self.is_dr_tool_download_started, dir_input);
            let open_dir = egui::Button::new("📁🔍");
            if ui
                .add_enabled(!self.is_dr_tool_download_started, open_dir)
                .on_hover_text("Find file dialog")
                .clicked()
            {
                // get current directory
                let current_dir = std::env::current_dir().unwrap();
                // open file dialog
                let fd = rfd::FileDialog::new()
                    .set_directory(&current_dir)
                    .add_filter("DynamoRIO tool (DrSymLogger.dll)", &["dll"]);
                if let Some(result) = fd.pick_file() {
                    self.settings.dr_tool_path = result.display().to_string();
                } else {
                    self.log_text.push_str("No valid file selected\n");
                }
            }
            let down_button = egui::Button::new("🌐⬇");
            if ui
                .add_enabled(!self.is_dr_tool_download_started, down_button)
                .on_hover_text(format!(
                    "Download from the internet to specified directory.\nUrl: {}",
                    DR_TOOL_DOWNLOAD_URL
                ))
                .clicked()
            {
                if self.is_dr_tool_path_ok {
                    self.log_text.push_str("DR tool path is already valid\n");
                    return;
                }

                // get current directory
                let dest_dir = std::env::current_dir().unwrap();
                let mut dest_dir = dest_dir.display().to_string();
                // open file dialog
                let fd = rfd::FileDialog::new().set_directory(&dest_dir);
                if let Some(result) = fd.pick_folder() {
                    dest_dir = result.display().to_string();
                }

                self.is_dr_tool_download_started = true;
                let tx = self.on_done_tool_down_tx.clone();
                let ctx2 = ctx.clone();
                log("spawning a thread\n");
                self.log_text.push_str(
                    format!(
                        "Download started {} -> {} ...\n",
                        DR_TOOL_DOWNLOAD_URL, dest_dir
                    )
                    .as_str(),
                );
                std::thread::spawn(move || {
                    log("thread started\n");
                    let mut resp = reqwest::blocking::get(DR_TOOL_DOWNLOAD_URL).unwrap();
                    log("get done\n");
                    assert!(resp.status().is_success());
                    let last_part = DR_TOOL_DOWNLOAD_URL.split('/').last().unwrap();
                    let mut out = std::fs::File::create(last_part).unwrap();
                    resp.copy_to(&mut out).unwrap();
                    log("file written\n");

                    ctx2.request_repaint();
                    let dest_dir = Path::new(&dest_dir).join(last_part);
                    let _ = tx.send(Some(dest_dir.display().to_string().to_owned()));
                });
            }
            if self.is_dr_tool_download_started {
                ui.spinner();
            }
            if !self.is_dr_tool_path_ok {
                ui.colored_label(egui::Color32::RED, "☹")
                    .on_hover_text("Invalid path");
            }
        });
        ui.end_row();
    }
}

impl eframe::App for MyApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array() // Make sure we don't paint anything behind the rounded corners
    }
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        custom_window_frame(ctx, frame, "DrSymLogger Launcher", |ui| {
            self.validate_fields_and_update_cmd();
            ui.horizontal(|ui| {
                egui::widgets::global_dark_light_mode_buttons(ui);
            });

            ui.heading("Options");
            egui::Grid::new("my_grid")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    self.show_dr_dir_row(ui, ctx);
                    self.show_tool_path_row(ui, ctx);

                    ui.label("Command line");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.settings.cmd);
                        if self.settings.cmd.is_empty() {
                            ui.colored_label(egui::Color32::RED, "☹")
                                .on_hover_text("Command line can't be empty");
                        }
                        if self.is_quote_in_cmd {
                            ui.colored_label(egui::Color32::RED, "☹")
                                .on_hover_text("No quotes currently supported, use console please");
                        }
                    });
                    ui.end_row();

                    ui.label("Instrumentation module");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.settings.inst_module);
                        if self.settings.inst_module.is_empty() {
                            ui.colored_label(egui::Color32::RED, "☹")
                                .on_hover_text("Module can't be empty");
                        }
                    });
                    ui.end_row();

                    ui.label("Instrumentation mode");
                    ui.horizontal(|ui| {
                        ui.radio_value(
                            &mut self.settings.inst_mode,
                            DrToolInstrumentationMode::Exec,
                            "Exec",
                        );
                        ui.radio_value(
                            &mut self.settings.inst_mode,
                            DrToolInstrumentationMode::Inst,
                            "Inst",
                        );
                    });
                    ui.end_row();

                    ui.label("Substring to match (optional, case sensitive)");
                    ui.horizontal(|ui| ui.text_edit_singleline(&mut self.settings.substr));
                    ui.end_row();

                    ui.label("Redirect to file (optional)");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.settings.redirect_to_file)
                    });
                    ui.end_row();
                });

            ui.separator();
            ui.horizontal(|ui| {
                ui.heading("Command line");
                if ui
                    .button("🗐")
                    .on_hover_text("Copy command line to clipboard")
                    .clicked()
                {
                    ctx.output_mut(|o| o.copied_text = self.cmd.clone());
                };
                if ui
                    .button("Run cdb")
                    .on_hover_text("Run cdb.exe to check symbols availability")
                    .clicked()
                {
                    let cdb_path =
                        "C:\\Program Files (x86)\\Windows Kits\\10\\Debuggers\\x64\\cdb.exe";
                    // split ext from module
                    let mut module = self.settings.inst_module.clone();
                    let mut _ext = "";
                    if let Some(pos) = module.rfind('.') {
                        _ext = &module[pos..];
                        module = module[..pos].to_string();
                    }
                    // if path exist
                    let cmd_args = format!("-c \"x {}!*\" {}", module, self.settings.cmd);
                    self.log_text
                        .push_str(format!("Running: {} {}\n", cdb_path, cmd_args).as_str());
                    // I didn't find a way to pass args as a single string, so I split it
                    // into a vector respecting quotes
                    let args_vec = Shlex::new(&cmd_args).collect::<Vec<_>>();
                    let child = std::process::Command::new(cdb_path).args(&args_vec).spawn();
                    if let Err(e) = child {
                        self.log_text.push_str(format!("Error: {}\n", e).as_str());
                    } else {
                        let exit_code = child.unwrap().wait().unwrap();
                        self.log_text
                            .push_str(format!("Exit code: {}\n", exit_code).as_str());
                    }
                }
                if ui
                    .button("Run")
                    .on_hover_text("Run the target process")
                    .clicked()
                {
                    // replace new line with &&
                    let cmd = self.cmd.replace("\n", "&&");
                    //let cmd = cmd.replace("\"", "\"\"");
                    self.log_text
                        .push_str(format!("Running: {}\n", cmd).as_str());
                    // run the process
                    let child = std::process::Command::new("cmd")
                        .args(&["/c", &cmd])
                        .spawn();
                    // check if error
                    if let Err(e) = child {
                        self.log_text.push_str(format!("Error: {}\n", e).as_str());
                    } else {
                        let exit_code = child.unwrap().wait().unwrap();
                        self.log_text
                            .push_str(format!("Exit code: {}\n", exit_code).as_str());
                    }
                };
            });
            ui.horizontal(|ui| {
                ui.style_mut().wrap = Some(true);
                ui.label(&self.cmd);
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.heading("Log");
                if ui.button("🗑").on_hover_text("Clear log").clicked() {
                    self.log_text.clear();
                };
                if ui
                    .button("🗐")
                    .on_hover_text("Copy log to clipboard")
                    .clicked()
                {
                    ctx.output_mut(|o| o.copied_text = self.log_text.clone());
                };
            });
            egui::ScrollArea::both()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.style_mut().wrap = Some(true);
                    ui.set_width(ui.available_width());
                    ui.set_min_height(ui.available_height());
                    for line in self.log_text.lines() {
                        ui.label(line);
                    }
                });
            // check if spawned thread sent data
            if let Ok(data) = self.on_done_dr_down_rc.try_recv() {
                self.settings.dr_dir = data.unwrap();
                self.is_dr_download_started = false;
            }
            if let Ok(data) = self.on_done_tool_down_rc.try_recv() {
                self.settings.dr_tool_path = data.unwrap();
                self.is_dr_tool_download_started = false;
            }
        });
    }
}

fn custom_window_frame(
    ctx: &egui::Context,
    frame: &mut eframe::Frame,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    use egui::*;

    let panel_frame = egui::Frame {
        fill: ctx.style().visuals.window_fill(),
        rounding: 10.0.into(),
        stroke: ctx.style().visuals.widgets.noninteractive.fg_stroke,
        outer_margin: 0.5.into(), // so the stroke is within the bounds
        ..Default::default()
    };

    CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
        let app_rect = ui.max_rect();

        let title_bar_height = 32.0;
        let title_bar_rect = {
            let mut rect = app_rect;
            rect.max.y = rect.min.y + title_bar_height;
            rect
        };
        title_bar_ui(ui, frame, title_bar_rect, title);

        // Add the contents:
        let content_rect = {
            let mut rect = app_rect;
            rect.min.y = title_bar_rect.max.y;
            rect
        }
        .shrink(4.0);
        let mut content_ui = ui.child_ui(content_rect, *ui.layout());
        add_contents(&mut content_ui);
    });
}

fn title_bar_ui(
    ui: &mut egui::Ui,
    frame: &mut eframe::Frame,
    title_bar_rect: eframe::epaint::Rect,
    title: &str,
) {
    use egui::*;

    let painter = ui.painter();

    let title_bar_response = ui.interact(title_bar_rect, Id::new("title_bar"), Sense::click());

    // Paint the title:
    painter.text(
        title_bar_rect.center(),
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(20.0),
        ui.style().visuals.text_color(),
    );

    // Paint the line under the title:
    painter.line_segment(
        [
            title_bar_rect.left_bottom() + vec2(1.0, 0.0),
            title_bar_rect.right_bottom() + vec2(-1.0, 0.0),
        ],
        ui.visuals().widgets.noninteractive.bg_stroke,
    );

    // Interact with the title bar (drag to move window):
    //if title_bar_response.double_clicked() {
    //    frame.set_maximized(!frame.info().window_info.maximized);
    //} else
    if title_bar_response.is_pointer_button_down_on() {
        frame.drag_window();
    }

    ui.allocate_ui_at_rect(title_bar_rect, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.visuals_mut().button_frame = false;
            ui.add_space(8.0);
            close_maximize_minimize(ui, frame);
        });
    });
}

/// Show some close/maximize/minimize buttons for the native window.
fn close_maximize_minimize(ui: &mut egui::Ui, frame: &mut eframe::Frame) {
    use egui::{Button, RichText};

    let button_height = 12.0;

    let close_response = ui
        .add(Button::new(RichText::new("🗙").size(button_height)))
        .on_hover_text("Close the window");
    if close_response.clicked() {
        frame.close();
    }

    // Disabled
    //if frame.info().window_info.maximized {
    //    let maximized_response = ui
    //        .add(Button::new(RichText::new("🗗").size(button_height)))
    //        .on_hover_text("Restore window");
    //    if maximized_response.clicked() {
    //        frame.set_maximized(false);
    //    }
    //} else {
    //    let maximized_response = ui
    //        .add(Button::new(RichText::new("🗗").size(button_height)))
    //        .on_hover_text("Maximize window");
    //    if maximized_response.clicked() {
    //        frame.set_maximized(true);
    //    }
    //}

    let minimized_response = ui
        .add(Button::new(RichText::new("🗕").size(button_height)))
        .on_hover_text("Minimize the window");
    if minimized_response.clicked() {
        frame.set_minimized(true);
    }
}

fn check_dr_dir(dr_dir: &str) -> bool {
    let dr_dir = Path::new(dr_dir);
    if !dr_dir.exists() {
        return false;
    }
    let dr_dir = dr_dir.join("bin64");
    if !dr_dir.exists() {
        return false;
    }
    let dr_dir = dr_dir.join("drrun.exe");
    if !dr_dir.exists() {
        return false;
    }
    true
}

fn check_dr_tool_path(dr_tool_path: &str) -> bool {
    // check if path exists, and the file name name is DrSymLogger.dll
    let dr_tool_path = Path::new(dr_tool_path);
    if !dr_tool_path.exists() {
        return false;
    }
    let dr_tool_path = dr_tool_path.file_name().unwrap();
    if dr_tool_path != "DrSymLogger.dll" {
        return false;
    }
    true
}

fn extract_zip_to_dir(input_path: &str, dest_dir: &str) {
    let file = std::fs::File::open(input_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let outpath = Path::new(dest_dir).join(file.mangled_name());
        if (&*file.name()).ends_with('/') {
            log(format!("File {} extracted to \"{}\"\n", i, outpath.display()).as_str());
            std::fs::create_dir_all(&outpath).unwrap();
        } else {
            log(format!(
                "File {} extracted to \"{}\" ({} bytes)\n",
                i,
                outpath.display(),
                file.size()
            )
            .as_str());
            if let Some(p) = outpath.parent() {
                if !p.exists() {
                    std::fs::create_dir_all(&p).unwrap();
                }
            }
            let mut outfile = std::fs::File::create(&outpath).unwrap();
            std::io::copy(&mut file, &mut outfile).unwrap();
        }
    }
}
