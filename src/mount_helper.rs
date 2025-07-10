use tracing::{info, error};

/// 将进程转为守护进程
pub fn daemonize() -> Result<(), String> {
    use nix::unistd::{fork, ForkResult, setsid};
    use nix::sys::stat::{umask, Mode};
    use std::env;
    
    // 第一次 fork
    match unsafe { fork() } {
        Ok(ForkResult::Parent { .. }) => {
            // 父进程退出，让 mount 命令返回
            std::process::exit(0);
        }
        Ok(ForkResult::Child) => {
            // 子进程继续
        }
        Err(e) => {
            return Err(format!("First fork failed: {}", e));
        }
    }
    
    // 创建新会话
    if let Err(e) = setsid() {
        return Err(format!("setsid failed: {}", e));
    }
    
    // 第二次 fork，确保不会获得控制终端
    match unsafe { fork() } {
        Ok(ForkResult::Parent { .. }) => {
            std::process::exit(0);
        }
        Ok(ForkResult::Child) => {
            // 孙进程继续
        }
        Err(e) => {
            return Err(format!("Second fork failed: {}", e));
        }
    }
    
    // 改变工作目录到根目录
    if let Err(e) = env::set_current_dir("/") {
        error!("Failed to change directory to /: {}", e);
    }
    
    // 设置文件权限掩码
    umask(Mode::from_bits(0o027).unwrap());
    
    // 重定向标准输入输出到 /dev/null
    let dev_null = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
    use std::os::unix::io::AsRawFd;
    
    unsafe {
        libc::dup2(dev_null.as_raw_fd(), 0); // stdin
        libc::dup2(dev_null.as_raw_fd(), 1); // stdout
        libc::dup2(dev_null.as_raw_fd(), 2); // stderr
    }
    
    info!("Successfully daemonized");
    Ok(())
}

/// 检查是否应该后台运行
pub fn should_daemonize(args: &[String]) -> bool {
    // 如果是 mount helper 模式且没有明确指定 foreground，则后台运行
    if args.len() > 0 && args[0].ends_with("mount.cachefs") {
        // 检查是否有 foreground 选项
        for i in 0..args.len() {
            if args[i] == "-o" && i + 1 < args.len() {
                let options = &args[i + 1];
                if options.contains("foreground") || options.contains("fg") {
                    return false;
                }
            }
        }
        return true;
    }
    false
}