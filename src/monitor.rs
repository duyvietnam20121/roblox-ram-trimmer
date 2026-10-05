//! Vòng lặp giám sát chính, chạy trên một thread nền riêng biệt với UI
//! thread của tray icon.
//!
//! Định kỳ đọc % RAM hệ thống; khi vượt ngưỡng thì kích hoạt một đợt trim
//! theo từng bước nhỏ, sau đó khóa (cooldown) trước khi cho phép trim lại.
//! Thread chờ bằng `recv_timeout` nên vừa ngủ thật sự (không tốn CPU) vừa
//! thức dậy ngay khi người dùng bấm "Trim ngay" trên tray.

use crate::app_state::{AppStateHandle, AppStatus, MonitorCommand, RamInfo};
use crate::config::{self, Settings};
use crate::roblox_process::{self, RobloxProcessError};
use crate::sysmem;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

enum MonitorPhase {
    /// Đang theo dõi bình thường, sẵn sàng trim khi cần.
    Watching,
    /// Vừa hoàn tất một đợt trim tại `since`, đang chờ hết cooldown.
    CoolingDown { since: Instant },
}

enum TrimCycleResult {
    /// Đã trim (hoặc thử trim và gặp lỗi có khả năng lặp lại) — vào cooldown.
    Trimmed,
    /// Không trim được gì (không thấy Roblox, bị hủy sớm...) — quay lại
    /// theo dõi ngay thay vì khóa cooldown cho một đợt chưa từng xảy ra.
    NotAttempted,
}

/// Chờ tới lần kiểm tra kế tiếp. Trả về `true` nếu bị đánh thức bởi lệnh
/// "trim ngay" của người dùng.
fn wait_for_next_tick(commands: &Receiver<MonitorCommand>, settings: &Settings) -> bool {
    match commands.recv_timeout(settings.ram_check_interval) {
        Ok(MonitorCommand::TrimNow) => true,
        Err(RecvTimeoutError::Timeout) => false,
        // Kênh đóng (không xảy ra khi app chạy bình thường): ngủ thường để
        // không biến vòng lặp thành busy-loop.
        Err(RecvTimeoutError::Disconnected) => {
            thread::sleep(settings.ram_check_interval);
            false
        }
    }
}

/// Chạy vòng lặp giám sát vô hạn. **Block** thread hiện tại — luôn gọi trên
/// một thread nền riêng, không bao giờ gọi trên UI thread.
pub fn run_monitor_loop(state: AppStateHandle, commands: Receiver<MonitorCommand>) -> ! {
    let settings = config::settings();
    let mut phase = MonitorPhase::Watching;
    // Đã ghi log cho chuỗi thất bại hiện tại chưa. Khi RAM cao do app khác
    // còn Roblox đang tắt (hoặc bị thiếu quyền), mỗi 3 giây ta lại thử và lại
    // thất bại — chỉ ghi log lần đầu để file log không phình ra từng ngày.
    let mut miss_logged = false;

    loop {
        let manual = wait_for_next_tick(&commands, settings);

        let ram_status = match sysmem::query_system_memory() {
            Ok(status) => status,
            Err(err) => {
                log::warn!("Không đọc được RAM hệ thống: {err}");
                state.set_status(AppStatus::Error {
                    message: "Không đọc được RAM hệ thống".to_string(),
                });
                continue;
            }
        };

        let ram = RamInfo {
            percent: ram_status.memory_load_percent as f32,
            available_bytes: ram_status.available_physical_bytes,
            total_bytes: ram_status.total_physical_bytes,
        };
        state.set_ram(ram);

        // Trim thủ công: bỏ qua ngưỡng, cooldown và cả trạng thái tạm dừng.
        if manual {
            log::info!("Người dùng yêu cầu trim ngay (RAM {:.0}%).", ram.percent);
            miss_logged = false;
            phase = match perform_trim_cycle(&state, settings, true, false) {
                TrimCycleResult::Trimmed => MonitorPhase::CoolingDown {
                    since: Instant::now(),
                },
                TrimCycleResult::NotAttempted => MonitorPhase::Watching,
            };
            // Bỏ các lệnh bấm dồn trong lúc đang trim, tránh trim lặp lại.
            while commands.try_recv().is_ok() {}
            continue;
        }

        if state.is_paused() {
            state.set_status(AppStatus::Paused);
            continue;
        }

        phase = match phase {
            MonitorPhase::CoolingDown { since } => {
                let elapsed = since.elapsed();
                if elapsed >= settings.cooldown_after_trim {
                    log::info!("Hết thời gian cooldown, tiếp tục theo dõi RAM.");
                    state.set_status(AppStatus::Monitoring);
                    MonitorPhase::Watching
                } else {
                    state.set_status(AppStatus::Cooldown {
                        remaining_secs: (settings.cooldown_after_trim - elapsed).as_secs(),
                    });
                    MonitorPhase::CoolingDown { since }
                }
            }
            MonitorPhase::Watching => {
                if ram.percent >= settings.ram_threshold_percent {
                    if !miss_logged {
                        log::info!(
                            "RAM hệ thống {:.0}% (còn {:.1}/{:.1} GB) >= ngưỡng {:.0}%, bắt đầu trim.",
                            ram.percent,
                            ram.available_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                            ram.total_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                            settings.ram_threshold_percent
                        );
                    }
                    match perform_trim_cycle(&state, settings, false, miss_logged) {
                        TrimCycleResult::Trimmed => {
                            miss_logged = false;
                            MonitorPhase::CoolingDown {
                                since: Instant::now(),
                            }
                        }
                        TrimCycleResult::NotAttempted => {
                            miss_logged = true;
                            MonitorPhase::Watching
                        }
                    }
                } else {
                    // RAM đã hạ xuống dưới ngưỡng: lần vượt ngưỡng sau sẽ được ghi log lại.
                    miss_logged = false;
                    state.set_status(AppStatus::Monitoring);
                    MonitorPhase::Watching
                }
            }
        };
    }
}

/// Chuyển lỗi tìm/trim tiến trình thành trạng thái hiển thị, kèm gợi ý
/// cụ thể khi nguyên nhân là thiếu quyền.
fn error_status(err: &RobloxProcessError, fallback: &str) -> AppStatus {
    let message = if err.is_access_denied() {
        "Không đủ quyền với Roblox — chạy app cùng mức quyền với Roblox".to_string()
    } else if matches!(err, RobloxProcessError::NotFound) {
        "Không tìm thấy tiến trình Roblox".to_string()
    } else {
        fallback.to_string()
    };
    AppStatus::Error { message }
}

/// Thực hiện một đợt trim trên tiến trình Roblox đang chiếm nhiều RAM nhất.
/// `forced` = trim thủ công (không bị hủy bởi trạng thái tạm dừng).
/// `quiet_misses` = bỏ qua log khi không tìm được Roblox (đã ghi ở lần trước).
fn perform_trim_cycle(
    state: &AppStateHandle,
    settings: &Settings,
    forced: bool,
    quiet_misses: bool,
) -> TrimCycleResult {
    let process = match roblox_process::find_largest_roblox_process() {
        Ok(p) => p,
        Err(err) => {
            if !quiet_misses {
                if matches!(err, RobloxProcessError::NotFound) {
                    log::info!("Cần trim nhưng không tìm thấy tiến trình Roblox nào đang chạy.");
                } else {
                    log::error!("Lỗi khi tìm tiến trình Roblox: {err}");
                }
            }
            state.set_status(error_status(&err, "Lỗi khi tìm tiến trình Roblox"));
            return TrimCycleResult::NotAttempted;
        }
    };

    log::info!(
        "Tiến trình mục tiêu: {} (PID {}), working set hiện tại: {:.1} MB",
        process.name,
        process.pid,
        process.working_set_bytes as f64 / 1024.0 / 1024.0
    );

    let total_target = settings.trim_total_target_bytes;
    let state_for_progress = state.clone();
    let state_for_cancel = state.clone();

    let trim_result = roblox_process::trim_process_gradually(
        &process,
        settings,
        move |requested| {
            let progress = ((requested as f64 / total_target as f64) * 100.0) as u8;
            state_for_progress.set_status(AppStatus::Trimming {
                progress_percent: progress.min(100),
            });
        },
        // Tạm dừng giữa chừng sẽ dừng sau bước hiện tại (trừ khi trim thủ công).
        move || !forced && state_for_cancel.is_paused(),
    );

    match trim_result {
        Ok(outcome) => {
            let measured = outcome.measured_freed_bytes;
            log::info!(
                "Trim xong: yêu cầu {:.1} MB, working set thực tế giảm {} trong {} bước \
                 (chạm sàn: {}, bị hủy: {}, EmptyWorkingSet: {}).",
                outcome.bytes_requested as f64 / 1024.0 / 1024.0,
                measured
                    .map(|b| format!("{:.1} MB", b as f64 / 1024.0 / 1024.0))
                    .unwrap_or_else(|| "không đo được".to_string()),
                outcome.steps_completed,
                outcome.hit_floor,
                outcome.cancelled,
                outcome.emptied_working_set
            );

            // Bị hủy trước khi trim được bước nào: không tính là một đợt trim.
            if outcome.cancelled && outcome.steps_completed == 0 {
                return TrimCycleResult::NotAttempted;
            }

            state.record_trim(
                process.name.clone(),
                measured.unwrap_or(outcome.bytes_requested),
                measured.is_some(),
            );
            state.set_status(AppStatus::Cooldown {
                remaining_secs: settings.cooldown_after_trim.as_secs(),
            });
            TrimCycleResult::Trimmed
        }
        Err(err) => {
            log::error!("Trim thất bại: {err}");
            state.set_status(error_status(&err, "Trim thất bại"));
            // Lỗi (ví dụ thiếu quyền) thường lặp lại ngay — vào cooldown để
            // không thử dồn dập mỗi vài giây gây tốn CPU.
            TrimCycleResult::Trimmed
        }
    }
}
