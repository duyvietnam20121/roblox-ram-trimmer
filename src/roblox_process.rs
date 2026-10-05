//! Tìm kiếm tiến trình Roblox đang chạy và thực hiện trim working set
//! (bộ nhớ vật lý đang được ánh xạ vào tiến trình) theo từng bước nhỏ.

use crate::config::{ROBLOX_PROCESS_NAMES, Settings};
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Memory::{SETPROCESSWORKINGSETSIZEEX_FLAGS, SetProcessWorkingSetSizeEx};
use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA, PROCESS_VM_READ,
};

/// Thời gian chờ cho Windows "ổn định" working set trước khi đo kết quả cuối.
const SETTLE_BEFORE_MEASURE: Duration = Duration::from_millis(500);

/// HRESULT của ERROR_ACCESS_DENIED (Win32 5 → 0x80070005).
const HRESULT_ACCESS_DENIED: u32 = 0x8007_0005;

/// `HANDLE` tự đóng khi ra khỏi phạm vi — không thể quên `CloseHandle`,
/// kể cả khi hàm thoát sớm bằng `?`.
struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: handle do WinAPI trả về, được sở hữu duy nhất bởi struct
        // này và chỉ bị đóng đúng một lần tại đây.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Thông tin tối thiểu về một tiến trình Roblox đang chạy.
#[derive(Debug, Clone)]
pub struct RobloxProcessInfo {
    pub pid: u32,
    pub name: String,
    /// Working set hiện tại (RAM vật lý thực sự đang chiếm dụng), tính bằng byte.
    pub working_set_bytes: u64,
}

/// Kết quả của một đợt trim.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrimOutcome {
    /// Tổng số byte đã YÊU CẦU Windows trim (cộng dồn các bước).
    pub bytes_requested: u64,
    /// Working set thực tế giảm được (trước − sau, đo sau khi nghỉ ngắn).
    /// Có thể nhỏ hơn `bytes_requested` nếu Roblox nạp lại trang ngay sau đó.
    pub measured_freed_bytes: Option<u64>,
    pub steps_completed: u32,
    /// Dừng sớm vì chạm sàn an toàn.
    pub hit_floor: bool,
    /// Dừng sớm vì người dùng tạm dừng giữa chừng.
    pub cancelled: bool,
    /// Có chạy bước `EmptyWorkingSet` tùy chọn thành công không.
    pub emptied_working_set: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RobloxProcessError {
    #[error("Không tìm thấy tiến trình Roblox nào đang chạy")]
    NotFound,
    #[error("Thấy {count} tiến trình Roblox nhưng không đủ quyền truy cập")]
    NotAccessible { count: usize },
    #[error("Không thể tạo snapshot danh sách tiến trình: {0}")]
    SnapshotFailed(windows::core::Error),
    #[error("Không thể mở handle tới tiến trình PID {pid}: {source}")]
    OpenProcessFailed {
        pid: u32,
        source: windows::core::Error,
    },
    #[error("Không thể đọc thông tin bộ nhớ của tiến trình PID {pid}: {source}")]
    QueryMemoryFailed {
        pid: u32,
        source: windows::core::Error,
    },
    #[error("Trim working set thất bại cho tiến trình PID {pid}: {source}")]
    TrimFailed {
        pid: u32,
        source: windows::core::Error,
    },
}

impl RobloxProcessError {
    /// Lỗi này có phải do thiếu quyền (Access Denied) không — dùng để gợi ý
    /// người dùng chạy app cùng mức quyền với Roblox.
    pub fn is_access_denied(&self) -> bool {
        match self {
            Self::NotAccessible { .. } => true,
            Self::OpenProcessFailed { source, .. }
            | Self::QueryMemoryFailed { source, .. }
            | Self::TrimFailed { source, .. } => source.code().0 as u32 == HRESULT_ACCESS_DENIED,
            _ => false,
        }
    }
}

/// Chuyển buffer UTF-16 có null-terminator thành `String`.
fn wide_str_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

/// Liệt kê các tiến trình Roblox đang chạy và truy cập được.
fn enumerate_roblox_processes() -> Result<Vec<RobloxProcessInfo>, RobloxProcessError> {
    // SAFETY: lời gọi WinAPI tiêu chuẩn để chụp snapshot danh sách tiến trình.
    let snapshot = OwnedHandle(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(RobloxProcessError::SnapshotFailed)?,
    );

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    let mut results = Vec::new();
    let mut denied = 0usize;

    // SAFETY: `entry` đã set đúng dwSize; `snapshot` là handle hợp lệ.
    let mut has_entry = unsafe { Process32FirstW(snapshot.raw(), &mut entry) }.is_ok();

    while has_entry {
        let process_name = wide_str_to_string(&entry.szExeFile);

        let is_roblox = ROBLOX_PROCESS_NAMES
            .iter()
            .any(|&name| name.eq_ignore_ascii_case(&process_name));

        if is_roblox {
            match query_working_set_size(entry.th32ProcessID) {
                Ok(working_set) => results.push(RobloxProcessInfo {
                    pid: entry.th32ProcessID,
                    name: process_name,
                    working_set_bytes: working_set,
                }),
                // Đếm tiến trình bị từ chối quyền để báo lỗi đúng nguyên nhân
                // thay vì nói chung chung "không tìm thấy Roblox".
                Err(err) if err.is_access_denied() => denied += 1,
                Err(_) => {}
            }
        }

        // SAFETY: cùng handle snapshot, entry được ghi đè tiếp tục hợp lệ.
        has_entry = unsafe { Process32NextW(snapshot.raw(), &mut entry) }.is_ok();
    }

    if results.is_empty() && denied > 0 {
        return Err(RobloxProcessError::NotAccessible { count: denied });
    }
    Ok(results)
}

/// Đọc working set hiện tại của một tiến trình qua một `HANDLE` đã mở sẵn
/// (tránh mở/đóng handle mới mỗi lần gọi trong vòng lặp trim).
fn query_working_set_size_with_handle(handle: HANDLE, pid: u32) -> Result<u64, RobloxProcessError> {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };

    // SAFETY: `handle` do caller đảm bảo hợp lệ và đủ quyền
    // PROCESS_QUERY_INFORMATION; `counters` đã set đúng trường `cb`.
    unsafe {
        GetProcessMemoryInfo(
            handle,
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    }
    .map_err(|source| RobloxProcessError::QueryMemoryFailed { pid, source })?;

    Ok(counters.WorkingSetSize as u64)
}

/// Đọc working set của một tiến trình theo PID (tự mở/đóng handle riêng).
fn query_working_set_size(pid: u32) -> Result<u64, RobloxProcessError> {
    // SAFETY: quyền tối thiểu chỉ để đọc thông tin bộ nhớ; PID lấy từ snapshot hệ thống.
    let handle = OwnedHandle(
        unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) }
            .map_err(|source| RobloxProcessError::OpenProcessFailed { pid, source })?,
    );
    query_working_set_size_with_handle(handle.raw(), pid)
}

/// Tìm tiến trình Roblox đang chiếm nhiều RAM nhất.
pub fn find_largest_roblox_process() -> Result<RobloxProcessInfo, RobloxProcessError> {
    enumerate_roblox_processes()?
        .into_iter()
        .max_by_key(|p| p.working_set_bytes)
        .ok_or(RobloxProcessError::NotFound)
}

/// Trim working set của tiến trình theo từng bước nhỏ
/// (`trim_step_bytes` mỗi lần, nghỉ `trim_step_interval` giữa các bước)
/// cho tới khi đạt `trim_total_target_bytes`, chạm sàn an toàn, hoặc bị hủy.
///
/// Đây là hành vi **best-effort**: `SetProcessWorkingSetSizeEx` chỉ ra lệnh
/// cho Windows đẩy các trang ít dùng ra khỏi working set; Roblox có thể
/// chạm lại các trang đó ngay sau. Vì vậy kết quả được đo lại thực tế
/// (`TrimOutcome::measured_freed_bytes`) thay vì tin con số đã yêu cầu.
///
/// - `on_step`: gọi sau mỗi bước với tổng số byte đã yêu cầu trim.
/// - `should_cancel`: kiểm tra trước mỗi bước; trả `true` để dừng sớm.
pub fn trim_process_gradually(
    process: &RobloxProcessInfo,
    settings: &Settings,
    mut on_step: impl FnMut(u64),
    should_cancel: impl Fn() -> bool,
) -> Result<TrimOutcome, RobloxProcessError> {
    // SAFETY: mở handle với đúng quyền cần thiết: đọc thông tin (re-query
    // working set giữa các bước) và set quota (bắt buộc cho trim).
    let handle = OwnedHandle(
        unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA | PROCESS_VM_READ,
                false,
                process.pid,
            )
        }
        .map_err(|source| RobloxProcessError::OpenProcessFailed {
            pid: process.pid,
            source,
        })?,
    );
    let raw = handle.raw();

    let initial_ws =
        query_working_set_size_with_handle(raw, process.pid).unwrap_or(process.working_set_bytes);

    let mut outcome = TrimOutcome::default();

    let trim_result = (|| -> Result<(), RobloxProcessError> {
        while outcome.bytes_requested < settings.trim_total_target_bytes {
            if should_cancel() {
                outcome.cancelled = true;
                break;
            }

            let current_ws =
                query_working_set_size_with_handle(raw, process.pid).unwrap_or(initial_ws);

            let remaining = settings.trim_total_target_bytes - outcome.bytes_requested;
            let step = settings.trim_step_bytes.min(remaining);

            // Không bao giờ trim xuống dưới sàn an toàn.
            if current_ws <= settings.min_working_set_floor_bytes {
                outcome.hit_floor = true;
                break;
            }

            let new_target = current_ws
                .saturating_sub(step)
                .max(settings.min_working_set_floor_bytes);
            let actual_step = current_ws - new_target;

            if actual_step == 0 {
                outcome.hit_floor = true;
                break;
            }

            // SAFETY: `raw` hợp lệ, mở với PROCESS_SET_QUOTA ở trên. flags=0:
            // giới hạn mềm, không ép cứng trần working set để Roblox không bị
            // từ chối cấp thêm RAM nếu cần đột ngột.
            unsafe {
                SetProcessWorkingSetSizeEx(
                    raw,
                    new_target as usize,
                    new_target as usize,
                    SETPROCESSWORKINGSETSIZEEX_FLAGS(0),
                )
            }
            .map_err(|source| RobloxProcessError::TrimFailed {
                pid: process.pid,
                source,
            })?;

            outcome.bytes_requested += actual_step;
            outcome.steps_completed += 1;
            on_step(outcome.bytes_requested);

            if outcome.bytes_requested < settings.trim_total_target_bytes {
                thread::sleep(settings.trim_step_interval);
            }
        }
        Ok(())
    })();

    trim_result?;

    // Tùy chọn (mặc định TẮT): EmptyWorkingSet tương đương
    // SetProcessWorkingSetSize(-1, -1), tức đẩy gần như TOÀN BỘ working set
    // ra khỏi RAM, bỏ qua mục tiêu và sàn an toàn ở trên.
    if settings.empty_working_set_after_trim && outcome.steps_completed > 0 {
        // SAFETY: `raw` còn hợp lệ, đủ quyền PROCESS_SET_QUOTA.
        outcome.emptied_working_set = unsafe { EmptyWorkingSet(raw) }.is_ok();
        if !outcome.emptied_working_set {
            log::warn!("EmptyWorkingSet thất bại cho PID {}, bỏ qua.", process.pid);
        }
    }

    // Đo lại working set thực tế để báo cáo trung thực.
    outcome.measured_freed_bytes = if outcome.steps_completed > 0 {
        thread::sleep(SETTLE_BEFORE_MEASURE);
        query_working_set_size_with_handle(raw, process.pid)
            .ok()
            .map(|final_ws| initial_ws.saturating_sub(final_ws))
    } else {
        Some(0)
    };

    Ok(outcome)
}
