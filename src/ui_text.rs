//! Các hàm thuần (không đụng WinAPI) phục vụ giao diện tray: định dạng
//! chuỗi, cắt tooltip, chọn kiểu icon theo trạng thái và sinh pixel icon.
//! Tách riêng khỏi `tray_ui` để kiểm thử được trên mọi nền tảng.

use crate::app_state::{AppStatus, LastTrimInfo, RamInfo};

/// Tooltip của Windows giới hạn ~127 ký tự; chừa dư một chút.
pub const TOOLTIP_MAX_CHARS: usize = 120;

/// Cắt chuỗi tối đa `max` ký tự (đếm theo ký tự, không phải byte — an toàn
/// với tiếng Việt), thêm dấu "…" nếu bị cắt.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / 1024.0
}

/// Dòng hiển thị RAM hệ thống trong menu.
pub fn format_ram_line(ram: Option<RamInfo>, threshold_percent: f32) -> String {
    match ram {
        Some(r) => format!(
            "RAM: {:.0}% — còn {:.1}/{:.1} GB (ngưỡng {:.0}%)",
            r.percent,
            gb(r.available_bytes),
            gb(r.total_bytes),
            threshold_percent
        ),
        None => "RAM hệ thống: đang đo...".to_string(),
    }
}

/// Dòng hiển thị lần trim gần nhất trong menu.
pub fn format_last_trim_line(last: Option<&LastTrimInfo>) -> String {
    match last {
        Some(info) => {
            let mb = info.bytes as f64 / 1024.0 / 1024.0;
            let what = if info.measured {
                "working set giảm"
            } else {
                "đã yêu cầu trim"
            };
            format!(
                "Lần trim cuối: {what} {mb:.0}MB — {} ({}s trước)",
                info.process_name,
                info.finished_at.elapsed().as_secs()
            )
        }
        None => "Chưa có lần trim nào".to_string(),
    }
}

/// Nội dung tooltip (đã cắt cho vừa giới hạn của Windows).
pub fn tooltip_text(status_text: &str) -> String {
    truncate_chars(&format!("Roblox RAM Trimmer\n{status_text}"), TOOLTIP_MAX_CHARS)
}

/// Kiểu icon hiển thị trên tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Idle,
    Busy,
    Cooldown,
    Error,
    Paused,
}

/// Chọn icon theo trạng thái. Đang trim luôn ưu tiên hiện "Busy" dù đã bấm
/// tạm dừng (vì đợt trim đang chạy dở vẫn đang tác động lên Roblox).
pub fn icon_kind_for(status: Option<&AppStatus>, paused: bool) -> IconKind {
    if let Some(AppStatus::Trimming { .. }) = status {
        return IconKind::Busy;
    }
    if paused {
        return IconKind::Paused;
    }
    match status {
        Some(AppStatus::Cooldown { .. }) => IconKind::Cooldown,
        Some(AppStatus::Error { .. }) => IconKind::Error,
        _ => IconKind::Idle,
    }
}

fn icon_color(kind: IconKind) -> [u8; 4] {
    match kind {
        IconKind::Idle => [64, 156, 255, 255],     // xanh dương
        IconKind::Busy => [255, 179, 0, 255],      // hổ phách
        IconKind::Cooldown => [158, 158, 158, 255], // xám
        IconKind::Error => [229, 57, 53, 255],     // đỏ
        IconKind::Paused => [96, 125, 139, 255],   // xám xanh
    }
}

/// Hai vạch "||" màu trắng cho icon tạm dừng — để phân biệt bằng hình dạng
/// chứ không chỉ bằng màu.
fn is_pause_bar(x: f32, y: f32, size: f32) -> bool {
    let in_height = y >= size * 0.28 && y < size * 0.72;
    let left = x >= size * 0.33 && x < size * 0.45;
    let right = x >= size * 0.55 && x < size * 0.67;
    in_height && (left || right)
}

/// Sinh dữ liệu RGBA (size × size) cho icon hình tròn đặc theo trạng thái.
pub fn icon_rgba(kind: IconKind, size: u32) -> Vec<u8> {
    let color = icon_color(kind);
    let mut buffer = Vec::with_capacity((size * size * 4) as usize);
    let center = size as f32 / 2.0;
    let radius = center - 1.0;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            if (dx * dx + dy * dy).sqrt() > radius {
                buffer.extend_from_slice(&[0, 0, 0, 0]); // trong suốt
            } else if kind == IconKind::Paused && is_pause_bar(x as f32, y as f32, size as f32) {
                buffer.extend_from_slice(&[255, 255, 255, 255]);
            } else {
                buffer.extend_from_slice(&color);
            }
        }
    }
    buffer
}
