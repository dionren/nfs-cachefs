// 时间工具的占位符实现
// 这个文件将在后续实现中完成

use std::time::SystemTime;

pub fn format_duration(duration: std::time::Duration) -> String {
    // 占位符实现
    format!("{:?}", duration)
}

pub fn system_time_to_timestamp(time: SystemTime) -> u64 {
    // 占位符实现
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
} 