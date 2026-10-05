//! Chỉ cho phép một bản app chạy tại một thời điểm trong mỗi phiên đăng
//! nhập Windows. Nếu chạy hai bản, cả hai cùng trim Roblox và hai icon tray
//! xuất hiện — vô ích và tốn tài nguyên.
//!
//! Cơ chế: tạo một named mutex. Nếu nó đã tồn tại (`ERROR_ALREADY_EXISTS`)
//! nghĩa là đã có bản khác đang chạy. Hệ điều hành tự giải phóng mutex khi
//! tiến trình kết thúc (kể cả khi crash), nên không bao giờ bị "kẹt".

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

/// Giữ mutex sống suốt vòng đời app; thả ra khi bị drop.
pub struct SingleInstanceGuard {
    handle: Option<HANDLE>,
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: `handle` do CreateMutexW trả về và chưa bị đóng ở đâu khác.
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}

/// Thử chiếm quyền chạy duy nhất. Trả về `None` nếu đã có bản khác đang chạy.
pub fn acquire() -> Option<SingleInstanceGuard> {
    // SAFETY: tên mutex là chuỗi wide hợp lệ kết thúc bằng null (macro `w!`);
    // không truyền SECURITY_ATTRIBUTES (None = mặc định).
    match unsafe { CreateMutexW(None, false, w!("Local\\RobloxRamTrimmer.SingleInstance")) } {
        Ok(handle) => {
            // SAFETY: đọc mã lỗi của thread hiện tại ngay sau CreateMutexW,
            // chưa có lời gọi API nào khác chen vào.
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                // SAFETY: handle vừa nhận được, đóng lại vì ta không giữ quyền chạy.
                unsafe {
                    let _ = CloseHandle(handle);
                }
                None
            } else {
                Some(SingleInstanceGuard {
                    handle: Some(handle),
                })
            }
        }
        // Không tạo được mutex (rất hiếm): vẫn cho chạy thay vì chặn người dùng.
        Err(_) => Some(SingleInstanceGuard { handle: None }),
    }
}
