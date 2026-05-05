//! Minimal sd_notify(3) client.
//!
//! We only need READY=1 after `/dev/cachefiles` is successfully bound. Keeping
//! this local avoids pulling in libsystemd or an extra crate for one datagram.

use std::io;

#[cfg(target_os = "linux")]
pub(crate) fn ready(status: &str) -> io::Result<bool> {
    use std::env;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    use std::path::Path;

    let Some(socket) = env::var_os("NOTIFY_SOCKET") else {
        return Ok(false);
    };
    if socket.is_empty() {
        return Ok(false);
    }

    let bytes = socket.as_bytes();
    let addr = if bytes.first() == Some(&b'@') {
        SocketAddr::from_abstract_name(&bytes[1..])?
    } else {
        SocketAddr::from_pathname(Path::new(&socket))?
    };

    let message = format!("READY=1\nSTATUS={status}");
    let sock = UnixDatagram::unbound()?;
    sock.send_to_addr(message.as_bytes(), &addr)?;
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn ready(_status: &str) -> io::Result<bool> {
    Ok(false)
}
