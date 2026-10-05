//! Trạng thái ứng dụng được chia sẻ an toàn giữa thread giám sát RAM
//! (background worker) và thread giao diện tray (UI thread).
//!
//! - Dữ liệu hiển thị (status, RAM, lần trim cuối) nằm sau một `Mutex` —
//!   tần suất cập nhật thấp (vài giây/lần) nên chi phí khóa không đáng kể.
//! - Cờ tạm dừng là `AtomicBool` (không cần khóa).
//! - Lệnh từ UI sang monitor (ví dụ "trim ngay") đi qua một kênh `mpsc`,
//!   để monitor thức dậy ngay thay vì phải chờ hết chu kỳ kiểm tra RAM.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Trạng thái hoạt động hiện tại của app, hiển thị trên tray tooltip/menu.
#[derive(Debug, Clone)]
pub enum AppStatus {
    /// Đang theo dõi RAM bình thường, chưa vượt ngưỡng.
    Monitoring,
    /// Người dùng đã tạm dừng chế độ tự động trim.
    Paused,
    /// Đang trim, kèm % tiến độ của đợt hiện tại.
    Trimming { progress_percent: u8 },
    /// Vừa trim xong, đang chờ hết cooldown.
    Cooldown { remaining_secs: u64 },
    /// Gặp lỗi (không tìm thấy Roblox, thiếu quyền, lỗi WinAPI...).
    Error { message: String },
}

impl AppStatus {
    /// Chuỗi hiển thị ngắn gọn cho tooltip/menu tray.
    pub fn display_text(&self) -> String {
        match self {
            AppStatus::Monitoring => "Đang theo dõi RAM".to_string(),
            AppStatus::Paused => "Đã tạm dừng tự động trim".to_string(),
            AppStatus::Trimming { progress_percent } => {
                format!("Đang trim RAM Roblox... {progress_percent}%")
            }
            AppStatus::Cooldown { remaining_secs } => {
                format!("Vừa trim xong — nghỉ {remaining_secs}s")
            }
            AppStatus::Error { message } => format!("Lỗi: {message}"),
        }
    }
}

/// Số liệu RAM hệ thống tại lần đo gần nhất.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RamInfo {
    pub percent: f32,
    pub available_bytes: u64,
    pub total_bytes: u64,
}

/// Thông tin về lần trim gần nhất, để hiển thị trong menu tray.
#[derive(Debug, Clone)]
pub struct LastTrimInfo {
    pub process_name: String,
    /// Số byte: working set thực tế giảm được nếu `measured`, ngược lại là
    /// số byte đã yêu cầu trim.
    pub bytes: u64,
    /// `true` nếu `bytes` là số đo thực tế (working set trước − sau).
    pub measured: bool,
    pub finished_at: Instant,
}

/// Dữ liệu hiển thị nằm sau mutex.
#[derive(Debug, Default)]
pub struct SharedAppState {
    pub status: Option<AppStatus>,
    pub ram: Option<RamInfo>,
    pub last_trim: Option<LastTrimInfo>,
}

/// Lệnh gửi từ UI sang thread monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorCommand {
    /// Trim ngay lập tức, bỏ qua ngưỡng RAM và cooldown.
    TrimNow,
}

/// Handle nhân bản được (rẻ) để truyền trạng thái giữa các thread.
#[derive(Debug, Clone)]
pub struct AppStateHandle {
    shared: Arc<Mutex<SharedAppState>>,
    paused: Arc<AtomicBool>,
    commands: Sender<MonitorCommand>,
}

impl AppStateHandle {
    /// Tạo handle cùng đầu nhận lệnh dành riêng cho thread monitor.
    pub fn create() -> (Self, Receiver<MonitorCommand>) {
        let (commands, receiver) = mpsc::channel();
        let handle = Self {
            shared: Arc::new(Mutex::new(SharedAppState::default())),
            paused: Arc::new(AtomicBool::new(false)),
            commands,
        };
        (handle, receiver)
    }

    /// Cập nhật trạng thái hoạt động hiện tại.
    pub fn set_status(&self, status: AppStatus) {
        if let Ok(mut state) = self.shared.lock() {
            state.status = Some(status);
        }
    }

    /// Cập nhật số liệu RAM hệ thống mới nhất.
    pub fn set_ram(&self, ram: RamInfo) {
        if let Ok(mut state) = self.shared.lock() {
            state.ram = Some(ram);
        }
    }

    /// Ghi nhận kết quả của một đợt trim vừa hoàn tất.
    pub fn record_trim(&self, process_name: String, bytes: u64, measured: bool) {
        if let Ok(mut state) = self.shared.lock() {
            state.last_trim = Some(LastTrimInfo {
                process_name,
                bytes,
                measured,
                finished_at: Instant::now(),
            });
        }
    }

    /// Đọc một bản sao trạng thái (khóa mutex đúng một lần).
    pub fn snapshot(&self) -> SharedAppStateSnapshot {
        match self.shared.lock() {
            Ok(state) => SharedAppStateSnapshot {
                status: state.status.clone(),
                ram: state.ram,
                last_trim: state.last_trim.clone(),
            },
            Err(_) => SharedAppStateSnapshot {
                status: None,
                ram: None,
                last_trim: None,
            },
        }
    }

    /// Chế độ tự động trim có đang bị tạm dừng không.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Đảo trạng thái tạm dừng, trả về trạng thái MỚI (`true` = đang tạm dừng).
    pub fn toggle_paused(&self) -> bool {
        !self.paused.fetch_xor(true, Ordering::Relaxed)
    }

    /// Yêu cầu monitor trim ngay (không chặn; bỏ qua lỗi nếu monitor đã dừng).
    pub fn request_trim_now(&self) {
        let _ = self.commands.send(MonitorCommand::TrimNow);
    }
}

/// Bản sao dữ liệu tại một thời điểm, tách khỏi khóa mutex.
#[derive(Debug, Clone)]
pub struct SharedAppStateSnapshot {
    pub status: Option<AppStatus>,
    pub ram: Option<RamInfo>,
    pub last_trim: Option<LastTrimInfo>,
}
