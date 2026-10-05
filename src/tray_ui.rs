//! Giao diện system tray: icon đổi theo trạng thái, tooltip và menu.
//!
//! `tray-icon` yêu cầu chạy trên cùng thread có một Win32 message loop
//! đang hoạt động. Ta tự bơm message tối giản (`PeekMessageW`) thay vì kéo
//! thêm winit/tao — giữ binary nhẹ và ít phụ thuộc.

use crate::app_state::{AppStateHandle, AppStatus};
use crate::config;
use crate::ui_text::{self, IconKind};
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

const MENU_ID_TRIM_NOW: &str = "trim_now";
const MENU_ID_PAUSE: &str = "pause";
const MENU_ID_QUIT: &str = "quit";

const ICON_SIZE: u32 = 32;

/// Chu kỳ "nhịp tim" vòng lặp UI: vừa là khoảng chờ sự kiện menu
/// (`recv_timeout` — thread ngủ thật sự), vừa là tần suất bơm message Win32.
const UI_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Tần suất cập nhật tooltip/menu/icon.
const UI_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// Các thành phần giao diện cần cập nhật định kỳ.
struct TrayWidgets {
    tray_icon: TrayIcon,
    status_item: MenuItem,
    ram_item: MenuItem,
    last_trim_item: MenuItem,
    pause_item: MenuItem,
    current_icon: IconKind,
}

/// Dựng `Icon` từ pixel sinh bằng code (không cần file .ico đi kèm).
fn build_icon(kind: IconKind) -> Option<Icon> {
    match Icon::from_rgba(ui_text::icon_rgba(kind, ICON_SIZE), ICON_SIZE, ICON_SIZE) {
        Ok(icon) => Some(icon),
        Err(err) => {
            log::error!("Không tạo được icon {kind:?}: {err:?}");
            None
        }
    }
}

/// Ghi log lỗi nghiêm trọng khi khởi tạo rồi thoát. App chạy ẩn và build
/// với `panic = "abort"` nên `expect()` sẽ chết mà không để lại dấu vết gì.
fn fatal(context: &str, err: impl std::fmt::Debug) -> ! {
    log::error!("{context}: {err:?}");
    std::process::exit(1);
}

/// Thêm một mục vào menu; lỗi thì ghi log rồi thoát (xem `fatal`).
macro_rules! append_or_die {
    ($menu:expr, $item:expr) => {
        if let Err(err) = $menu.append($item) {
            fatal("Không thể dựng menu tray", err);
        }
    };
}

fn pause_menu_text(paused: bool) -> &'static str {
    if paused {
        "Tiếp tục tự động trim"
    } else {
        "Tạm dừng tự động trim"
    }
}

/// Khởi tạo tray icon + menu, rồi chạy vòng lặp UI vô hạn.
///
/// **Block** thread hiện tại — chạy trên main thread, trong khi việc giám
/// sát RAM chạy trên thread nền (xem `monitor::run_monitor_loop`).
pub fn run_tray_ui(state: AppStateHandle) -> ! {
    let settings = config::settings();

    // Các dòng thông tin được đánh dấu disabled: chỉ là nhãn, không click được.
    let status_item = MenuItem::new("Đang khởi động...", false, None);
    let ram_item = MenuItem::new(
        ui_text::format_ram_line(None, settings.ram_threshold_percent),
        false,
        None,
    );
    let last_trim_item = MenuItem::new(ui_text::format_last_trim_line(None), false, None);
    let trim_now_item = MenuItem::with_id(MENU_ID_TRIM_NOW, "Trim Roblox ngay", true, None);
    let pause_item = MenuItem::with_id(MENU_ID_PAUSE, pause_menu_text(false), true, None);
    let quit_item = MenuItem::with_id(MENU_ID_QUIT, "Thoát", true, None);

    let menu = Menu::new();
    append_or_die!(menu, &status_item);
    append_or_die!(menu, &ram_item);
    append_or_die!(menu, &last_trim_item);
    append_or_die!(menu, &PredefinedMenuItem::separator());
    append_or_die!(menu, &trim_now_item);
    append_or_die!(menu, &pause_item);
    append_or_die!(menu, &PredefinedMenuItem::separator());
    append_or_die!(menu, &quit_item);

    let initial_icon = match build_icon(IconKind::Idle) {
        Some(icon) => icon,
        None => fatal("Không thể dựng icon tray", "dữ liệu RGBA không hợp lệ"),
    };

    let tray_icon = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Roblox RAM Trimmer — đang khởi động...")
        .with_icon(initial_icon)
        .build()
    {
        Ok(tray_icon) => tray_icon,
        Err(err) => fatal("Không thể tạo tray icon", err),
    };

    let mut widgets = TrayWidgets {
        tray_icon,
        status_item,
        ram_item,
        last_trim_item,
        pause_item,
        current_icon: IconKind::Idle,
    };

    let menu_event_receiver = MenuEvent::receiver();
    let tray_event_receiver = TrayIconEvent::receiver();

    let mut msg = MSG::default();
    let mut last_ui_refresh = Instant::now();

    loop {
        // Bơm hết message Win32 đang chờ (bắt buộc để Shell_NotifyIcon và
        // menu popup hoạt động).
        // SAFETY: `msg` là buffer hợp lệ của thread hiện tại; đây là vòng
        // lặp message tiêu chuẩn của ứng dụng Win32.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // Click trực tiếp lên icon: chưa cần xử lý, chỉ tiêu thụ để kênh không đầy.
        let _ = tray_event_receiver.try_recv();

        if last_ui_refresh.elapsed() >= UI_REFRESH_INTERVAL {
            refresh_ui(&state, &mut widgets);
            last_ui_refresh = Instant::now();
        }

        // Chờ sự kiện menu tối đa UI_POLL_INTERVAL — thread thực sự NGỦ
        // trong lúc chờ và trả về ngay khi có click (không có độ trễ nhân tạo).
        if let Ok(event) = menu_event_receiver.recv_timeout(UI_POLL_INTERVAL) {
            match event.id.0.as_str() {
                MENU_ID_QUIT => {
                    log::info!("Người dùng chọn Thoát từ tray menu.");
                    std::process::exit(0);
                }
                MENU_ID_TRIM_NOW => {
                    log::info!("Người dùng bấm Trim Roblox ngay.");
                    state.request_trim_now();
                }
                MENU_ID_PAUSE => {
                    let paused = state.toggle_paused();
                    log::info!(
                        "Tự động trim: {}.",
                        if paused { "tạm dừng" } else { "tiếp tục" }
                    );
                    // Cập nhật giao diện ngay thay vì chờ chu kỳ refresh kế tiếp.
                    refresh_ui(&state, &mut widgets);
                    last_ui_refresh = Instant::now();
                }
                _ => {}
            }
        }
    }
}

/// Đọc trạng thái mới nhất rồi cập nhật tooltip, menu và icon.
/// Chỉ khóa mutex đúng một lần (qua `snapshot`).
fn refresh_ui(state: &AppStateHandle, widgets: &mut TrayWidgets) {
    let snapshot = state.snapshot();
    let paused = state.is_paused();

    let status_text = snapshot
        .status
        .as_ref()
        .map(AppStatus::display_text)
        .unwrap_or_else(|| "Đang khởi động...".to_string());

    widgets.status_item.set_text(&status_text);
    widgets.ram_item.set_text(ui_text::format_ram_line(
        snapshot.ram,
        config::settings().ram_threshold_percent,
    ));
    widgets
        .last_trim_item
        .set_text(ui_text::format_last_trim_line(snapshot.last_trim.as_ref()));
    widgets.pause_item.set_text(pause_menu_text(paused));

    let _ = widgets
        .tray_icon
        .set_tooltip(Some(ui_text::tooltip_text(&status_text)));

    // Chỉ đổi icon khi kiểu icon thực sự thay đổi (hiếm), tránh gọi
    // Shell_NotifyIcon dày đặc không cần thiết.
    let desired = ui_text::icon_kind_for(snapshot.status.as_ref(), paused);
    if desired != widgets.current_icon {
        if let Some(icon) = build_icon(desired) {
            if widgets.tray_icon.set_icon(Some(icon)).is_ok() {
                widgets.current_icon = desired;
            }
        }
    }
}
