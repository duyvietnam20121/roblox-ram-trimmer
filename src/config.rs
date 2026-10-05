//! Cấu hình trung tâm cho toàn bộ ứng dụng.
//!
//! Giá trị mặc định nằm trong `Settings::default()`. Người dùng có thể ghi
//! đè bằng file `roblox-ram-trimmer.ini` đặt cạnh file .exe (định dạng
//! `key = value`, tự parse — không kéo thêm thư viện để binary vẫn nhỏ).
//! Giá trị sai hoặc ngoài khoảng cho phép bị bỏ qua, dùng lại mặc định và
//! ghi cảnh báo vào log thay vì làm app không chạy được.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// Tên các tiến trình Roblox hợp lệ (không phân biệt hoa/thường).
pub const ROBLOX_PROCESS_NAMES: &[&str] = &["RobloxPlayerBeta.exe", "RobloxStudioBeta.exe"];

/// Tên file log (đặt trong `%LOCALAPPDATA%`).
pub const LOG_FILE_NAME: &str = "roblox-ram-trimmer.log";

/// Log vượt ngưỡng này khi khởi động sẽ được đổi tên thành `.log.old`.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Tên file cấu hình tùy chọn, đặt cạnh file .exe.
pub const SETTINGS_FILE_NAME: &str = "roblox-ram-trimmer.ini";

const MB: u64 = 1024 * 1024;

/// Toàn bộ thông số điều chỉnh hành vi của app.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Ngưỡng % RAM hệ thống để bắt đầu trim.
    pub ram_threshold_percent: f32,
    /// Tổng dung lượng muốn trim mỗi đợt.
    pub trim_total_target_bytes: u64,
    /// Kích thước mỗi bước trim nhỏ (chia nhỏ để tránh giật lag).
    pub trim_step_bytes: u64,
    /// Thời gian nghỉ giữa hai bước trim liên tiếp.
    pub trim_step_interval: Duration,
    /// Tần suất kiểm tra RAM hệ thống.
    pub ram_check_interval: Duration,
    /// Thời gian khóa sau một đợt trim trước khi cho phép trim lại.
    pub cooldown_after_trim: Duration,
    /// Sàn an toàn: không bao giờ trim working set của Roblox xuống dưới mức này.
    pub min_working_set_floor_bytes: u64,
    /// Gọi thêm `EmptyWorkingSet` sau khi trim xong. MẶC ĐỊNH TẮT: API này
    /// đẩy gần như toàn bộ working set ra khỏi RAM (bỏ qua mục tiêu 512MB
    /// và sàn an toàn) nên Roblox sẽ phải nạp lại nhiều trang → dễ giật lag.
    pub empty_working_set_after_trim: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ram_threshold_percent: 85.0,
            trim_total_target_bytes: 512 * MB,
            trim_step_bytes: 32 * MB,
            trim_step_interval: Duration::from_millis(1500),
            ram_check_interval: Duration::from_secs(3),
            cooldown_after_trim: Duration::from_secs(60),
            min_working_set_floor_bytes: 128 * MB,
            empty_working_set_after_trim: false,
        }
    }
}

fn parse_in_range(value: &str, min: u64, max: u64) -> Option<u64> {
    value.parse::<u64>().ok().filter(|v| (min..=max).contains(v))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn range_warning(line_no: usize, key: &str, value: &str, expected: &str) -> String {
    format!("dòng {line_no}: {key} phải là {expected}, nhận '{value}' — dùng giá trị mặc định")
}

impl Settings {
    /// Parse nội dung file .ini. Trả về cấu hình cùng danh sách cảnh báo
    /// (dòng sai cú pháp, khóa lạ, giá trị ngoài khoảng cho phép).
    pub fn parse(text: &str) -> (Settings, Vec<String>) {
        // Notepad có thể lưu file kèm BOM UTF-8; bỏ đi để khóa đầu tiên không bị hỏng.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);

        let mut s = Settings::default();
        let mut warnings = Vec::new();

        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                warnings.push(format!("dòng {line_no}: thiếu dấu '=' ({line})"));
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            // Cho phép chú thích cuối dòng: `cooldown_secs = 60  # nghỉ 1 phút`.
            let value = value
                .split(['#', ';'])
                .next()
                .unwrap_or("")
                .trim();

            match key.as_str() {
                "ram_threshold_percent" => match value.parse::<f32>() {
                    Ok(v) if (1.0..=99.0).contains(&v) => s.ram_threshold_percent = v,
                    _ => warnings.push(range_warning(line_no, &key, value, "số từ 1 đến 99")),
                },
                "trim_total_mb" => match parse_in_range(value, 1, 8192) {
                    Some(v) => s.trim_total_target_bytes = v * MB,
                    None => warnings.push(range_warning(line_no, &key, value, "số nguyên từ 1 đến 8192")),
                },
                "trim_step_mb" => match parse_in_range(value, 1, 1024) {
                    Some(v) => s.trim_step_bytes = v * MB,
                    None => warnings.push(range_warning(line_no, &key, value, "số nguyên từ 1 đến 1024")),
                },
                "trim_step_interval_ms" => match parse_in_range(value, 100, 60_000) {
                    Some(v) => s.trim_step_interval = Duration::from_millis(v),
                    None => warnings.push(range_warning(line_no, &key, value, "số nguyên từ 100 đến 60000")),
                },
                "ram_check_interval_secs" => match parse_in_range(value, 1, 300) {
                    Some(v) => s.ram_check_interval = Duration::from_secs(v),
                    None => warnings.push(range_warning(line_no, &key, value, "số nguyên từ 1 đến 300")),
                },
                "cooldown_secs" => match parse_in_range(value, 0, 3600) {
                    Some(v) => s.cooldown_after_trim = Duration::from_secs(v),
                    None => warnings.push(range_warning(line_no, &key, value, "số nguyên từ 0 đến 3600")),
                },
                "min_working_set_floor_mb" => match parse_in_range(value, 0, 4096) {
                    Some(v) => s.min_working_set_floor_bytes = v * MB,
                    None => warnings.push(range_warning(line_no, &key, value, "số nguyên từ 0 đến 4096")),
                },
                "empty_working_set_after_trim" => match parse_bool(value) {
                    Some(v) => s.empty_working_set_after_trim = v,
                    None => warnings.push(range_warning(line_no, &key, value, "true hoặc false")),
                },
                _ => warnings.push(format!("dòng {line_no}: khóa không hợp lệ '{key}', bỏ qua")),
            }
        }

        // Kiểm tra chéo: một bước không được lớn hơn tổng mục tiêu.
        if s.trim_step_bytes > s.trim_total_target_bytes {
            warnings.push(
                "trim_step_mb lớn hơn trim_total_mb — hạ bước xuống bằng tổng mục tiêu".to_string(),
            );
            s.trim_step_bytes = s.trim_total_target_bytes;
        }

        (s, warnings)
    }

    /// Đọc file `roblox-ram-trimmer.ini` cạnh .exe. Không có file là bình
    /// thường (dùng mặc định, không cảnh báo); lỗi đọc khác được báo cảnh báo.
    pub fn load_beside_exe() -> (Settings, Vec<String>) {
        let Some(path) = settings_path() else {
            return (Settings::default(), Vec::new());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Settings::parse(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                (Settings::default(), Vec::new())
            }
            Err(err) => (
                Settings::default(),
                vec![format!("không đọc được {}: {err}", path.display())],
            ),
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(SETTINGS_FILE_NAME))
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Đăng ký cấu hình dùng chung cho toàn app (gọi một lần ở `main`, TRƯỚC
/// mọi lời gọi `settings()`). Trả về `false` nếu cấu hình đã được thiết lập
/// từ trước — khi đó giá trị mới bị bỏ qua.
pub fn init(settings: Settings) -> bool {
    SETTINGS.set(settings).is_ok()
}

/// Truy cập cấu hình toàn cục; chưa `init` thì dùng giá trị mặc định.
pub fn settings() -> &'static Settings {
    SETTINGS.get_or_init(Settings::default)
}
