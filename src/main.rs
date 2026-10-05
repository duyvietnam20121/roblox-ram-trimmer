//! Roblox RAM Trimmer
//!
//! Ứng dụng nền chạy trên system tray: khi RAM hệ thống vượt ngưỡng (mặc
//! định 85%), tự động trim working set của tiến trình Roblox đang chiếm
//! nhiều RAM nhất, chia thành nhiều bước nhỏ (mặc định 32MB/bước, cách nhau
//! 1.5s, tổng 512MB) để tránh spike I/O/CPU gây giật lag khi đang chơi.
//!
//! # Kiến trúc
//! - `config`: giá trị mặc định + đọc ghi đè từ file `.ini` cạnh .exe.
//! - `sysmem`: đọc % RAM toàn hệ thống qua WinAPI.
//! - `roblox_process`: tìm tiến trình Roblox, trim working set, đo kết quả.
//! - `app_state`: trạng thái chia sẻ giữa các thread + kênh lệnh UI → monitor.
//! - `monitor`: vòng lặp giám sát trên thread nền (ngưỡng, cooldown, pause).
//! - `tray_ui` / `ui_text`: giao diện tray (main thread) và các hàm định dạng thuần.
//! - `single_instance`: chặn chạy hai bản cùng lúc.
//! - `logfile`: xoay vòng file log.
//!
//! # Giới hạn cần biết
//! `SetProcessWorkingSetSizeEx` chỉ ra lệnh cho Windows đẩy các trang ít
//! dùng ra khỏi working set — best-effort, không phải cam kết cứng. Roblox
//! có thể chạm lại các trang đó ngay sau. App đo working set trước/sau và
//! báo số thực tế trên tray.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Release build: ẩn cửa sổ console (chỉ hiện tray icon). Debug build vẫn
// giữ console để xem log trực tiếp.

mod app_state;
mod config;
mod logfile;
mod monitor;
mod roblox_process;
mod single_instance;
mod sysmem;
mod tray_ui;
mod ui_text;

use app_state::AppStateHandle;

/// Stack cho thread monitor. Công việc của nó nhẹ (đọc RAM, liệt kê tiến
/// trình, ghi log) nên 256 KiB là dư; mặc định của Rust (2 MiB) là phí.
const MONITOR_STACK_BYTES: usize = 256 * 1024;

fn main() {
    // Phải chạy TRƯỚC khi đụng tới file log: bản thứ hai không được phép
    // mở/làm hỏng file log của bản đang chạy.
    let Some(_instance_guard) = single_instance::acquire() else {
        return;
    };

    init_logging();

    let (settings, warnings) = config::Settings::load_beside_exe();
    for warning in &warnings {
        log::warn!("Cấu hình: {warning}");
    }
    if !config::init(settings) {
        log::warn!("Cấu hình đã được khởi tạo từ trước, bỏ qua file .ini.");
    }

    let settings = config::settings();
    log::info!("=== Roblox RAM Trimmer khởi động ===");
    log::info!(
        "Cấu hình: ngưỡng RAM {}%, trim {}MB/đợt, bước {}MB, nghỉ {}ms/bước, cooldown {}s, \
         kiểm tra mỗi {}s, sàn working set {}MB, EmptyWorkingSet: {}",
        settings.ram_threshold_percent,
        settings.trim_total_target_bytes / 1024 / 1024,
        settings.trim_step_bytes / 1024 / 1024,
        settings.trim_step_interval.as_millis(),
        settings.cooldown_after_trim.as_secs(),
        settings.ram_check_interval.as_secs(),
        settings.min_working_set_floor_bytes / 1024 / 1024,
        settings.empty_working_set_after_trim
    );

    let (state, commands) = AppStateHandle::create();

    // Monitor chạy trên thread nền riêng: việc trim (kéo dài vài chục giây
    // vì chia nhỏ) không bao giờ làm treo tray icon.
    let monitor_state = state.clone();
    let spawned = std::thread::Builder::new()
        .name("ram-monitor".to_string())
        .stack_size(MONITOR_STACK_BYTES)
        .spawn(move || {
            monitor::run_monitor_loop(monitor_state, commands);
        });
    if let Err(err) = spawned {
        log::error!("Không thể tạo thread giám sát RAM: {err}");
        return;
    }

    // Tray UI phải chạy trên main thread vì nó sở hữu Win32 message loop.
    tray_ui::run_tray_ui(state);
}

/// Ghi log ra file trong `%LOCALAPPDATA%` (app chạy ẩn nên đây là cách duy
/// nhất để chẩn đoán sự cố). Log quá lớn sẽ được xoay vòng trước khi mở.
fn init_logging() {
    let log_dir = std::env::var("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());

    let log_path = log_dir.join(config::LOG_FILE_NAME);

    logfile::rotate_if_too_big(&log_path, config::MAX_LOG_BYTES);

    // Không mở được file log (ví dụ do quyền) thì app vẫn chạy bình thường.
    if let Err(err) = simple_logging::log_to_file(&log_path, log::LevelFilter::Info) {
        eprintln!("Không thể khởi tạo file log tại {log_path:?}: {err}");
    }
}
