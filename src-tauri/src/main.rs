#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analyzer;
mod disk;
mod scanner;

use scanner::{ScanOptions, ScanSnapshot};
use serde_json::Value;
use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

#[derive(Clone, Copy, PartialEq)]
enum Operation {
    Idle,
    Scanning,
    Cleaning,
    Analyzing,
    Choosing,
}

struct Session {
    operation: Operation,
    snapshot: Option<ScanSnapshot>,
    analyses: VecDeque<analyzer::AnalysisReport>,
}

struct CleanerState {
    session: Arc<Mutex<Session>>,
    cancel: Arc<AtomicBool>,
    home: PathBuf,
}

fn home_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("无法读取用户目录")?;
    let canonical = home
        .canonicalize()
        .map_err(|e| format!("无法访问用户目录：{e}"))?;
    if !canonical.is_dir() || canonical.parent().is_none() {
        return Err("用户目录无效".into());
    }
    Ok(canonical)
}

#[tauri::command]
fn get_disk_overview(state: State<'_, CleanerState>) -> Result<Value, String> {
    let (total_bytes, available_bytes) = analyzer::filesystem_capacity(&state.home)?;
    Ok(serde_json::json!({"totalBytes":total_bytes,"availableBytes":available_bytes}))
}

#[tauri::command]
fn get_analysis_locations(state: State<'_, CleanerState>) -> Value {
    let home = &state.home;
    serde_json::json!([
        {"id":"home", "label":"我的文件", "path":home},
        {"id":"data", "label":"整个磁盘", "path":"/System/Volumes/Data"},
        {"id":"caches", "label":"缓存（临时文件）", "path":home.join("Library/Caches")},
        {"id":"logs", "label":"运行记录", "path":home.join("Library/Logs")},
        {"id":"downloads", "label":"下载的文件", "path":home.join("Downloads")},
        {"id":"app-support", "label":"应用保存的数据", "path":home.join("Library/Application Support")}
    ])
}

fn analysis_value(report: &analyzer::AnalysisReport) -> Result<Value, String> {
    let mut value = serde_json::to_value(report).map_err(|error| error.to_string())?;
    match disk::storage_summary(&PathBuf::from(&report.root.path)) {
        Ok(storage) => {
            value["storage"] = serde_json::to_value(storage).map_err(|error| error.to_string())?;
        }
        Err(error) => {
            value["storage"] = Value::Null;
            value["storageWarning"] = Value::String(error);
        }
    }
    Ok(value)
}

#[tauri::command]
fn open_privacy_settings() -> Result<(), String> {
    let status = Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("无法打开系统设置，请手动打开隐私与安全性中的完全磁盘访问权限".into())
    }
}

#[tauri::command]
async fn choose_analysis_directory(
    app: tauri::AppHandle,
    state: State<'_, CleanerState>,
) -> Result<Option<String>, String> {
    let session = state.session.clone();
    {
        let mut guard = session.lock().map_err(|_| "操作状态不可用")?;
        if guard.operation != Operation::Idle {
            return Err("请先等待当前操作完成".into());
        }
        guard.operation = Operation::Choosing;
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("选择要分析占用的文件夹")
            .blocking_pick_folder()
            .map(|file| {
                file.into_path()
                    .map_err(|e| e.to_string())
                    .and_then(|path| {
                        path.to_str()
                            .map(str::to_owned)
                            .ok_or("路径不是有效 UTF-8".into())
                    })
            })
            .transpose()
    })
    .await;
    session.lock().map_err(|_| "操作状态不可用")?.operation = Operation::Idle;
    result.map_err(|e| format!("文件夹选择未完成：{e}"))?
}

#[tauri::command]
async fn analyze_directory(
    app: tauri::AppHandle,
    state: State<'_, CleanerState>,
    path: String,
) -> Result<Value, String> {
    let session = state.session.clone();
    let cancel = state.cancel.clone();
    {
        let mut guard = session.lock().map_err(|_| "分析状态不可用")?;
        if guard.operation != Operation::Idle {
            return Err("已有扫描或清理正在进行".into());
        }
        guard.operation = Operation::Analyzing;
        cancel.store(false, Ordering::Release);
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        let report = analyzer::analyze(&PathBuf::from(path), &cancel, |progress| {
            let _ = app.emit("analysis-progress", progress);
        })?;
        let value = analysis_value(&report)?;
        Ok::<_, String>((report, value))
    })
    .await;
    let mut guard = session.lock().map_err(|_| "分析状态不可用")?;
    guard.operation = Operation::Idle;
    match result {
        Ok(Ok((report, value))) => {
            guard.analyses.push_front(report);
            guard.analyses.truncate(8);
            Ok(value)
        }
        Ok(Err(error)) => Err(error),
        Err(error) => Err(format!("目录分析未完成：{error}")),
    }
}

#[tauri::command]
fn cancel_analysis(state: State<'_, CleanerState>) -> Result<(), String> {
    let guard = state.session.lock().map_err(|_| "分析状态不可用")?;
    if guard.operation == Operation::Analyzing {
        state.cancel.store(true, Ordering::Release);
    }
    Ok(())
}

fn find_node<'a>(
    node: &'a analyzer::DirectoryNode,
    id: &str,
) -> Option<&'a analyzer::DirectoryNode> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_node(child, id))
}

#[tauri::command]
fn reveal_analysis_node(
    state: State<'_, CleanerState>,
    analysis_id: String,
    node_id: String,
) -> Result<(), String> {
    let path = {
        let guard = state.session.lock().map_err(|_| "分析状态不可用")?;
        let report = guard
            .analyses
            .iter()
            .find(|report| report.analysis_id == analysis_id)
            .ok_or("此分析结果已过期，请重新扫描目录")?;
        find_node(&report.root, &node_id)
            .ok_or("此目录未包含在分析结果中")?
            .path
            .clone()
    };
    if path.is_empty() {
        return Err("此路径不能用 UTF-8 表示，无法从应用定位；大小已经统计".into());
    }
    let status = Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("无法在 Finder 中显示项目".into())
    }
}

#[tauri::command]
async fn start_scan(
    app: tauri::AppHandle,
    state: State<'_, CleanerState>,
    options: ScanOptions,
) -> Result<Value, String> {
    let session = state.session.clone();
    let cancel = state.cancel.clone();
    let home = state.home.clone();
    {
        let mut guard = session.lock().map_err(|_| "扫描状态不可用")?;
        if guard.operation != Operation::Idle {
            return Err("已有扫描或清理正在进行".into());
        }
        guard.operation = Operation::Scanning;
        cancel.store(false, Ordering::Release);
    }
    let work = tauri::async_runtime::spawn_blocking(move || {
        scanner::scan(&home, options, &cancel, |progress| {
            let _ = app.emit("scan-progress", progress);
        })
    })
    .await;
    let mut guard = session.lock().map_err(|_| "扫描状态不可用")?;
    guard.operation = Operation::Idle;
    match work {
        Ok(Ok(snapshot)) => {
            let report = serde_json::to_value(&snapshot.report).map_err(|e| e.to_string())?;
            guard.snapshot = Some(snapshot);
            Ok(report)
        }
        Ok(Err(error)) => Err(error),
        Err(error) => Err(format!("扫描未完成：{error}")),
    }
}

#[tauri::command]
fn cancel_scan(state: State<'_, CleanerState>) -> Result<(), String> {
    let guard = state.session.lock().map_err(|_| "扫描状态不可用")?;
    if guard.operation == Operation::Scanning {
        state.cancel.store(true, Ordering::Release);
    }
    Ok(())
}

#[tauri::command]
fn get_last_scan(state: State<'_, CleanerState>) -> Result<Value, String> {
    let guard = state.session.lock().map_err(|_| "扫描状态不可用")?;
    guard
        .snapshot
        .as_ref()
        .map(|snapshot| serde_json::to_value(&snapshot.report))
        .transpose()
        .map(|v| v.unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn clean_items(
    app: tauri::AppHandle,
    state: State<'_, CleanerState>,
    scan_id: String,
    item_ids: Vec<String>,
) -> Result<Value, String> {
    let session = state.session.clone();
    let mut snapshot = {
        let mut guard = session.lock().map_err(|_| "清理状态不可用")?;
        if guard.operation != Operation::Idle {
            return Err("已有扫描或清理正在进行".into());
        }
        let existing = guard.snapshot.as_ref().ok_or("请先扫描")?;
        if existing.report.scan_id != scan_id {
            return Err("扫描结果已更新，请重新选择文件".into());
        }
        if item_ids.is_empty() {
            return Err("请先选择要清理的项目".into());
        }
        if item_ids.iter().collect::<HashSet<_>>().len() != item_ids.len() {
            return Err("清理列表包含重复项目".into());
        }
        if item_ids.len() > existing.report.items.len()
            || item_ids
                .iter()
                .any(|id| !existing.report.items.iter().any(|item| &item.id == id))
        {
            return Err("清理列表包含无效项目，请重新扫描".into());
        }
        guard.operation = Operation::Cleaning;
        guard.snapshot.take().ok_or("请先扫描")?
    };
    let work = tauri::async_runtime::spawn_blocking(move || {
        let result = scanner::cleanup(&mut snapshot, &item_ids, |progress| {
            let _ = app.emit("cleanup-progress", progress);
        });
        (snapshot, result)
    })
    .await;
    let mut guard = session.lock().map_err(|_| "清理状态不可用")?;
    guard.operation = Operation::Idle;
    match work {
        Ok((snapshot, result)) => {
            guard.snapshot = Some(snapshot);
            result.and_then(|report| serde_json::to_value(report).map_err(|e| e.to_string()))
        }
        Err(error) => Err(format!("清理中断，请重新扫描并检查废纸篓：{error}")),
    }
}

#[tauri::command]
fn reveal_item(
    state: State<'_, CleanerState>,
    scan_id: String,
    item_id: String,
) -> Result<(), String> {
    let path = {
        let guard = state.session.lock().map_err(|_| "扫描状态不可用")?;
        let snapshot = guard.snapshot.as_ref().ok_or("请先扫描")?;
        if snapshot.report.scan_id != scan_id {
            return Err("扫描结果已更新".into());
        }
        snapshot
            .report
            .items
            .iter()
            .find(|item| item.id == item_id)
            .ok_or("项目已不存在")?
            .path
            .clone()
    };
    let status = Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("无法在 Finder 中显示项目".into())
    }
}

#[tauri::command]
fn open_trash() -> Result<(), String> {
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", "tell application \"Finder\" to open trash"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "无法打开废纸篓：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn is_cleaning(app: &tauri::AppHandle) -> bool {
    app.try_state::<CleanerState>()
        .and_then(|state| {
            state
                .session
                .lock()
                .ok()
                .map(|session| session.operation == Operation::Cleaning)
        })
        .unwrap_or(false)
}

fn native_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let about = PredefinedMenuItem::about(app, Some("关于 Mac Sweep"), None)?;
    let settings = MenuItem::with_id(app, "settings", "扫描设置…", true, Some("CmdOrCtrl+,"))?;
    let application = Submenu::with_items(
        app,
        "Mac Sweep",
        true,
        &[
            &about,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, Some("隐藏 Mac Sweep"))?,
            &PredefinedMenuItem::hide_others(app, Some("隐藏其他应用"))?,
            &PredefinedMenuItem::show_all(app, Some("显示全部应用"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some("退出 Mac Sweep"))?,
        ],
    )?;
    let scan = MenuItem::with_id(app, "scan", "开始检查", true, Some("CmdOrCtrl+R"))?;
    let cancel = MenuItem::with_id(app, "cancel", "停止扫描", true, Some("CmdOrCtrl+."))?;
    let trash = MenuItem::with_id(app, "open-trash", "打开废纸篓", true, None::<&str>)?;
    let file = Submenu::with_items(
        app,
        "文件",
        true,
        &[
            &scan,
            &cancel,
            &PredefinedMenuItem::separator(app)?,
            &trash,
            &PredefinedMenuItem::close_window(app, Some("关闭窗口"))?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        "编辑",
        true,
        &[
            &PredefinedMenuItem::undo(app, Some("撤销"))?,
            &PredefinedMenuItem::redo(app, Some("重做"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, Some("剪切"))?,
            &PredefinedMenuItem::copy(app, Some("复制"))?,
            &PredefinedMenuItem::paste(app, Some("粘贴"))?,
            &PredefinedMenuItem::select_all(app, Some("全选文本"))?,
        ],
    )?;
    let home = MenuItem::with_id(app, "home", "我的 Mac", true, Some("CmdOrCtrl+1"))?;
    let all = MenuItem::with_id(app, "all", "清理建议", true, Some("CmdOrCtrl+2"))?;
    let analysis = MenuItem::with_id(app, "analysis", "空间去哪里了", true, Some("CmdOrCtrl+3"))?;
    let safe = MenuItem::with_id(
        app,
        "select-safe",
        "选择建议项",
        true,
        Some("CmdOrCtrl+Shift+A"),
    )?;
    let view = Submenu::with_items(
        app,
        "查看",
        true,
        &[
            &home,
            &all,
            &analysis,
            &PredefinedMenuItem::separator(app)?,
            &safe,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        "窗口",
        true,
        &[
            &PredefinedMenuItem::minimize(app, Some("最小化"))?,
            &PredefinedMenuItem::maximize(app, Some("缩放"))?,
        ],
    )?;
    Menu::with_items(app, &[&application, &file, &edit, &view, &window])
}

fn main() {
    let home = home_dir().expect("Mac Sweep needs a valid home directory");
    if std::env::args().nth(1).as_deref() == Some("--scan-json") {
        let cancel = AtomicBool::new(false);
        match scanner::scan(&home, ScanOptions::default(), &cancel, |_| {}) {
            Ok(snapshot) => println!(
                "{}",
                serde_json::to_string_pretty(&snapshot.report).expect("serializable scan")
            ),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--analyze-json") {
        let path = std::env::args_os()
            .nth(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| home.clone());
        let show_progress = std::env::var("MAC_SWEEP_SCAN_PROGRESS").as_deref() == Ok("1");
        let last_progress = std::cell::Cell::new(std::time::Instant::now());
        match analyzer::analyze(&path, &AtomicBool::new(false), |progress| {
            if show_progress && last_progress.get().elapsed() >= std::time::Duration::from_secs(5) {
                eprintln!(
                    "Scanned {} files; measured {} bytes",
                    progress.scanned_files, progress.bytes_found
                );
                last_progress.set(std::time::Instant::now());
            }
        }) {
            Ok(report) => println!(
                "{}",
                serde_json::to_string_pretty(
                    &analysis_value(&report).expect("serializable analysis")
                )
                .expect("serializable analysis")
            ),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.set_menu(native_menu(app.handle())?)?;
            Ok(())
        })
        .manage(CleanerState {
            home,
            session: Arc::new(Mutex::new(Session {
                operation: Operation::Idle,
                snapshot: None,
                analyses: VecDeque::new(),
            })),
            cancel: Arc::new(AtomicBool::new(false)),
        })
        .invoke_handler(tauri::generate_handler![
            start_scan,
            cancel_scan,
            get_last_scan,
            clean_items,
            reveal_item,
            open_trash,
            get_analysis_locations,
            get_disk_overview,
            choose_analysis_directory,
            analyze_directory,
            cancel_analysis,
            reveal_analysis_node,
            open_privacy_settings
        ])
        .on_menu_event(|app, event| {
            let id = event.id.as_ref();
            if matches!(
                id,
                "scan"
                    | "cancel"
                    | "settings"
                    | "select-safe"
                    | "all"
                    | "home"
                    | "analysis"
                    | "open-trash"
            ) {
                let _ = app.emit("menu-action", id);
            }
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if is_cleaning(window.app_handle()) {
                    api.prevent_close();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("Could not build Mac Sweep");
    app.run(|handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if is_cleaning(handle) {
                api.prevent_exit();
            }
        }
    });
}
