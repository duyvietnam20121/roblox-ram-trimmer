//! Quản lý kích thước file log: app chạy nền nhiều ngày nên không thể để
//! file log phình vô hạn.

use std::path::Path;

/// Nếu file log lớn hơn `max_bytes`, đổi tên nó thành `<tên>.log.old`
/// (ghi đè bản `.old` cũ). Trả về `true` nếu đã xoay vòng.
///
/// Gọi TRƯỚC khi mở file log để ghi. Mọi lỗi (file không tồn tại, không có
/// quyền...) đều bị bỏ qua — thiếu xoay vòng không phải lý do để dừng app.
pub fn rotate_if_too_big(path: &Path, max_bytes: u64) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > max_bytes => {
            let old = path.with_extension("log.old");
            std::fs::rename(path, old).is_ok()
        }
        _ => false,
    }
}
