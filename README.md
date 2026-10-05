# Roblox RAM Trimmer

App nền chạy trên system tray (Windows). Khi **RAM toàn hệ thống** vượt
ngưỡng (mặc định **85%**), app trim **512MB** working set của **tiến trình
Roblox đang chiếm nhiều RAM nhất**, chia thành nhiều bước nhỏ (32MB/bước,
cách nhau 1.5s) để tránh spike CPU/I/O gây giật lag khi đang chơi.
App chỉ tác động lên tiến trình Roblox, không đụng tiến trình khác.

## Tính năng

- Trim từng bước nhỏ, một đợt mỗi lần vượt ngưỡng, rồi nghỉ (cooldown 60s).
- **Đo kết quả thực tế**: app so working set của Roblox trước và sau khi trim
  và hiển thị con số đo được (không chỉ con số đã yêu cầu).
- Icon tray đổi màu theo trạng thái: xanh = theo dõi, vàng = đang trim,
  xám = cooldown, đỏ = lỗi, xám xanh có hai vạch = tạm dừng.
- Menu tray: **Trim Roblox ngay**, **Tạm dừng/Tiếp tục tự động trim**, **Thoát**,
  cùng các dòng trạng thái / RAM (còn bao nhiêu GB) / lần trim cuối.
- File cấu hình `.ini` tùy chọn, không cần build lại để đổi ngưỡng/tốc độ.
- Chỉ chạy được một bản tại một thời điểm (tránh hai bản cùng trim Roblox).
- Báo rõ khi thiếu quyền truy cập Roblox thay vì nói "không tìm thấy".
- File log tự xoay vòng khi vượt 1MB.

## Yêu cầu

- Windows 10/11 (dùng WinAPI, không chạy trên Linux/macOS).
- Rust 1.98.0, edition 2024 (nếu tự build).
- App và Roblox nên chạy cùng user session và cùng mức quyền. Nếu Roblox chạy
  quyền cao hơn app, `OpenProcess` bị từ chối và tray sẽ báo
  "Không đủ quyền với Roblox".

## Build

Cục bộ:

```powershell
cargo build --release
```

Binary: `target\release\roblox-ram-trimmer.exe` (một file độc lập, icon sinh bằng code).
Bản debug (`cargo run`) giữ console để xem log trực tiếp.

GitHub Actions (`.github/workflows/build.yml`, runner `windows-latest`):

- Push lên `main` hoặc bấm **Run workflow**: build, tải artifact ở tab Actions.
- Push tag `v*` (ví dụ `git tag v0.2.0 && git push origin v0.2.0`): build và
  tự tạo GitHub Release đính kèm `.exe`.
- Lần build đầu sinh `Cargo.lock`; hãy commit file này để các lần sau tái lập được.

## Chạy

Double-click `.exe`, hoặc đặt shortcut vào `shell:startup` để tự chạy cùng Windows.
Log ghi tại `%LOCALAPPDATA%\roblox-ram-trimmer.log` (bản cũ: `.log.old`).

## Cấu hình (tùy chọn)

Đổi tên `roblox-ram-trimmer.ini.example` thành `roblox-ram-trimmer.ini`, đặt
**cạnh file .exe**, rồi chạy lại app. Khóa không có trong file dùng giá trị mặc định;
giá trị sai bị bỏ qua và cảnh báo được ghi vào log.

| Khóa | Mặc định | Ý nghĩa |
|---|---|---|
| `ram_threshold_percent` | 85 | Ngưỡng % RAM hệ thống để kích hoạt trim |
| `trim_total_mb` | 512 | Tổng dung lượng trim mỗi đợt |
| `trim_step_mb` | 32 | Dung lượng mỗi bước trim |
| `trim_step_interval_ms` | 1500 | Nghỉ giữa các bước |
| `ram_check_interval_secs` | 3 | Tần suất kiểm tra RAM |
| `cooldown_secs` | 60 | Nghỉ sau mỗi đợt trim |
| `min_working_set_floor_mb` | 128 | Sàn an toàn, không trim thấp hơn |
| `empty_working_set_after_trim` | false | Xem cảnh báo bên dưới |

## Cách hoạt động

1. Mỗi 3 giây đọc % RAM hệ thống (`GlobalMemoryStatusEx`).
2. Khi RAM ≥ ngưỡng, tìm tiến trình Roblox có working set lớn nhất.
3. Trim từng bước: mỗi bước đặt giới hạn working set thấp hơn hiện tại một
   khoảng bằng `trim_step_mb` (không bao giờ xuống dưới sàn), nghỉ giữa các bước.
4. Nghỉ ngắn rồi đo lại working set để báo số giảm thực tế.
5. Vào cooldown. Nếu không có Roblox để trim thì **không** cooldown, thử lại ở lần kiểm tra kế tiếp.

## Giới hạn kỹ thuật cần biết

- Trim working set là **best-effort**. Windows chỉ đẩy các trang ít dùng ra
  khỏi working set của Roblox; nếu Roblox chạm lại chúng ngay sau đó, số đo
  thực tế sẽ thấp hơn số đã yêu cầu. Đó là lý do tray hiển thị số **đo được**.
- Trim working set **không chống phân mảnh bộ nhớ vật lý**. Nó chỉ chuyển trang
  ra khỏi working set (sang standby/page file). Cách giảm rủi ro hiệu quả nhất
  là không trim quá tay: chia bước nhỏ, giữ sàn 128MB và có cooldown.
- `empty_working_set_after_trim = true` gọi `EmptyWorkingSet`, tương đương
  `SetProcessWorkingSetSize(-1, -1)`: đẩy gần như **toàn bộ** working set ra khỏi RAM,
  bỏ qua mục tiêu 512MB và sàn an toàn. Vì vậy nó **tắt mặc định**.

## Tối ưu tài nguyên

- Vòng lặp UI và vòng lặp monitor đều chờ bằng `recv_timeout`: thread ngủ thật sự
  (không busy-poll) và thức dậy ngay khi có sự kiện/lệnh.
- Thread monitor dùng stack 256 KiB thay vì mặc định.
- Handle tiến trình mở một lần cho cả đợt trim và tự đóng (RAII).
- Mỗi lần làm mới giao diện chỉ khóa mutex một lần; icon chỉ đổi khi trạng thái đổi.
- Log không ghi mỗi chu kỳ; chỉ ghi khi có sự kiện (trim, lỗi, tạm dừng/tiếp tục), và
  chuỗi thất bại lặp lại (ví dụ Roblox đang tắt) chỉ được ghi lần đầu.
- Không dùng thư viện parse cấu hình; profile release bật LTO, strip, `panic = "abort"`.

## Cấu trúc mã nguồn

```
src/
├── main.rs             entry point, khởi tạo, spawn thread
├── config.rs           Settings + đọc file .ini
├── sysmem.rs           đọc RAM hệ thống
├── roblox_process.rs   tìm Roblox, trim, đo kết quả
├── app_state.rs        trạng thái chia sẻ + kênh lệnh UI -> monitor
├── monitor.rs          vòng lặp giám sát (ngưỡng, cooldown, pause)
├── tray_ui.rs          tray icon, menu, vòng lặp UI
├── ui_text.rs          hàm định dạng/icon thuần (có thể test)
├── single_instance.rs  chặn chạy hai bản
└── logfile.rs          xoay vòng log
```

## Ý tưởng tiếp theo

- Mục menu bật/tắt "chạy cùng Windows" (ghi registry Run).
- Thông báo balloon khi trim xong.
- Test tích hợp WinAPI trên Windows CI (hiện chỉ các module thuần được test tự động).
