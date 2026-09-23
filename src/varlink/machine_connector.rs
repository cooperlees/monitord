//! Varlink connections into machines, made from inside their PID namespace.
//!
//! systemd's credential-checking varlink servers (PID 1, networkd) refuse
//! peers they cannot see in their own PID namespace: `SO_PEERCRED` reports
//! pid 0 for a process in an ancestor namespace and the server closes the
//! connection (see #211 and systemd/systemd#43807). Reaching a machine's
//! sockets through `/proc/<leader>/root` is therefore not enough — the
//! `connect()` itself has to happen inside the machine's PID namespace.
//!
//! Joining a PID namespace needs `CAP_SYS_ADMIN` (and opening it
//! `CAP_SYS_PTRACE`), and `setns(CLONE_NEWPID)` only moves future children.
//! So per machine we spawn one small helper — monitord re-executed with
//! [`HELPER_ARG`] — from a short-lived thread that joined the namespace. The
//! helper opens the machine's root directory, drops to an unprivileged user
//! without any capabilities, and then serves connect requests over a
//! socketpair: monitord names one of a fixed set of sockets
//! ([`MachineSocket`]) and receives a connected fd back via `SCM_RIGHTS`. The
//! kernel records peer credentials at `connect()` time, so the fd keeps
//! working in monitord, which never leaves its own namespaces.
//!
//! Connectors are cached per machine and leader PID (see `crate::machines`),
//! so a machine costs one helper spawn rather than one per collection cycle,
//! and each varlink connection only a request round trip over the socketpair.

use std::fmt;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;

/// Hidden first argument that makes monitord run as a machine connector
/// helper instead of a collector. Dispatched from `main` before the tokio
/// runtime starts; see [`helper_main`].
pub const HELPER_ARG: &str = "__monitord-machine-connector";

/// The machine sockets a connector may connect to.
///
/// A fixed allowlist sent as a single byte, rather than arbitrary paths, so a
/// helper can only ever be used to reach these read-mostly systemd endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MachineSocket {
    /// PID 1's `io.systemd.Manager`, `io.systemd.Unit` and `io.systemd.Job`.
    Manager = 1,
    /// PID 1's `io.systemd.Metrics` report socket.
    Metrics = 2,
    /// systemd-networkd's `io.systemd.Network`.
    Network = 3,
}

impl MachineSocket {
    pub const ALL: [MachineSocket; 3] = [Self::Manager, Self::Metrics, Self::Network];

    /// Socket path inside the machine.
    pub fn path(self) -> &'static str {
        match self {
            Self::Manager => crate::varlink::manager::MANAGER_SOCKET_PATH,
            Self::Metrics => crate::varlink_units::METRICS_SOCKET_PATH,
            Self::Network => crate::varlink::network::NETWORK_SOCKET_PATH,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|socket| *socket as u8 == byte)
    }
}

#[derive(Error, Debug)]
pub enum MachineConnectorError {
    /// monitord lacks the privileges to join the machine's PID namespace.
    /// Won't change at runtime, so callers stop trying for every machine.
    #[error(
        "joining the PID namespace of machine leader {leader_pid} was refused ({source}); \
         varlink collection from machines needs CAP_SYS_ADMIN and CAP_SYS_PTRACE"
    )]
    PermissionDenied { leader_pid: u32, source: io::Error },
    #[error("machine connector for leader {leader_pid}: {source}")]
    Io { leader_pid: u32, source: io::Error },
    #[error("machine connectors are only supported on Linux")]
    Unsupported,
}

/// A helper inside one machine's PID namespace handing out connected sockets.
pub struct MachineConnector {
    leader_pid: u32,
    control: Mutex<OwnedFd>,
    child: Mutex<std::process::Child>,
}

impl fmt::Debug for MachineConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MachineConnector")
            .field("leader_pid", &self.leader_pid)
            .finish_non_exhaustive()
    }
}

impl MachineConnector {
    /// Spawn a connector for the machine whose leader is `leader_pid`.
    ///
    /// Blocking: joins the namespace on a short-lived thread, spawns the
    /// helper and waits (at most `timeout`) until it reports that it opened
    /// the machine's root and dropped its privileges.
    pub fn spawn(leader_pid: u32, timeout: Duration) -> Result<Self, MachineConnectorError> {
        sys::spawn(leader_pid, timeout)
    }

    pub fn leader_pid(&self) -> u32 {
        self.leader_pid
    }

    /// Whether the helper is still running. It only exits on its own when
    /// its machine goes away or on error, so a cached connector that is no
    /// longer alive must be replaced rather than reused.
    pub fn is_alive(&self) -> bool {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        matches!(child.try_wait(), Ok(None))
    }

    /// Connect to `socket` inside the machine, blocking for the round trip.
    pub fn connect_blocking(
        &self,
        socket: MachineSocket,
    ) -> Result<std::os::unix::net::UnixStream, MachineConnectorError> {
        let control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        sys::request(&control, socket).map_err(|source| MachineConnectorError::Io {
            leader_pid: self.leader_pid,
            source,
        })
    }

    /// Connect to `socket` inside the machine and wrap it for zlink.
    ///
    /// The tokio stream is created on the calling runtime, so this also works
    /// from the per-call current-thread runtimes the streaming collectors use.
    pub async fn connect(
        self: &Arc<Self>,
        socket: MachineSocket,
    ) -> anyhow::Result<zlink::unix::Connection> {
        let this = Arc::clone(self);
        let stream = tokio::task::spawn_blocking(move || this.connect_blocking(socket)).await??;
        stream.set_nonblocking(true)?;
        let stream = tokio::net::UnixStream::from_std(stream)?;
        Ok(zlink::unix::Connection::new(stream.into()))
    }
}

impl Drop for MachineConnector {
    fn drop(&mut self) {
        // The helper also exits on EOF once `control` closes; killing it
        // just makes that immediate, and waiting reaps it.
        let child = self.child.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Entry point of the helper process. Never returns.
pub fn helper_main(args: &[String]) -> ! {
    let code = match sys::helper_run(args) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("monitord machine connector: {err}");
            1
        }
    };
    std::process::exit(code)
}

/// Whether `err` means monitord cannot join machine namespaces at all.
pub fn is_permission_denied(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<MachineConnectorError>(),
        Some(MachineConnectorError::PermissionDenied { .. })
    )
}

#[cfg(target_os = "linux")]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::time::Duration;

    use super::{MachineConnector, MachineConnectorError, MachineSocket, HELPER_ARG};

    /// The helper never connects as root: if monitord runs as root, the
    /// helper drops to nobody, so a machine always sees an unprivileged peer.
    const NOBODY: libc::uid_t = 65534;

    pub(super) fn spawn(
        leader_pid: u32,
        timeout: Duration,
    ) -> Result<MachineConnector, MachineConnectorError> {
        let io_err = |source| MachineConnectorError::Io { leader_pid, source };
        let perm_or_io = |source: io::Error| match source.raw_os_error() {
            Some(libc::EPERM) | Some(libc::EACCES) => {
                MachineConnectorError::PermissionDenied { leader_pid, source }
            }
            _ => MachineConnectorError::Io { leader_pid, source },
        };

        // Opening another user's namespace needs ptrace access (CAP_SYS_PTRACE).
        let pidns = File::open(format!("/proc/{leader_pid}/ns/pid")).map_err(perm_or_io)?;
        let (control, helper_end) = seqpacket_pair().map_err(io_err)?;
        set_recv_timeout(control.as_fd(), timeout).map_err(io_err)?;

        // setns(CLONE_NEWPID) only changes the PID namespace of the calling
        // thread's future children, so join it on a throwaway thread and spawn
        // the helper from there: the helper starts inside the machine's PID
        // namespace, and the change dies with the thread instead of lingering
        // on a runtime worker.
        let child = std::thread::scope(|scope| {
            scope
                .spawn(|| -> io::Result<std::process::Child> {
                    // SAFETY: plain syscall on a valid fd we own.
                    if unsafe { libc::setns(pidns.as_raw_fd(), libc::CLONE_NEWPID) } != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Command::new("/proc/self/exe")
                        .arg(HELPER_ARG)
                        .arg(leader_pid.to_string())
                        .env_clear()
                        .stdin(Stdio::from(helper_end))
                        .stdout(Stdio::null())
                        .stderr(Stdio::inherit())
                        .spawn()
                })
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("namespace join thread panicked")))
        })
        .map_err(perm_or_io)?;

        let connector = MachineConnector {
            leader_pid,
            control: Mutex::new(control),
            child: Mutex::new(child),
        };

        // Wait for the helper's ready message: root opened, privileges dropped.
        let control = connector.control.lock().unwrap_or_else(|e| e.into_inner());
        let (status, _) = recv_status(control.as_fd()).map_err(io_err)?;
        drop(control);
        if status != 0 {
            return Err(perm_or_io(io::Error::from_raw_os_error(status.into())));
        }
        Ok(connector)
    }

    pub(super) fn request(control: &OwnedFd, socket: MachineSocket) -> io::Result<UnixStream> {
        let byte = [socket as u8];
        // SAFETY: valid fd and a 1 byte buffer that outlives the call.
        let sent = unsafe {
            libc::send(
                control.as_raw_fd(),
                byte.as_ptr().cast(),
                byte.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        match recv_status(control.as_fd())? {
            (0, Some(fd)) => Ok(UnixStream::from(fd)),
            (0, None) => Err(io::Error::other("connector replied without a socket")),
            (errno, _) => Err(io::Error::from_raw_os_error(errno.into())),
        }
    }

    pub(super) fn helper_run(args: &[String]) -> io::Result<()> {
        let leader_pid: u32 = args
            .first()
            .and_then(|arg| arg.parse().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing leader PID"))?;
        // SAFETY: the parent passes the helper's end of the socketpair as stdin.
        let control = unsafe { OwnedFd::from_raw_fd(0) };

        let root = match open_root(leader_pid).and_then(|root| drop_privileges().map(|()| root)) {
            Ok(root) => root,
            Err(err) => {
                let _ = send_status(control.as_fd(), errno_byte(&err), None);
                return Err(err);
            }
        };
        send_status(control.as_fd(), 0, None)?;
        serve(control.as_fd(), root.as_fd())
    }

    /// Answer connect requests until monitord closes its end.
    ///
    /// Separate from [`helper_run`] so it can be tested without privileges,
    /// against any directory standing in for a machine's root.
    pub(super) fn serve(control: BorrowedFd<'_>, root: BorrowedFd<'_>) -> io::Result<()> {
        loop {
            let mut byte = [0u8; 1];
            // SAFETY: valid fd and a 1 byte buffer that outlives the call.
            let n = unsafe { libc::recv(control.as_raw_fd(), byte.as_mut_ptr().cast(), 1, 0) };
            match n {
                0 => return Ok(()),
                n if n < 0 => {
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(err);
                }
                _ => {}
            }
            let Some(socket) = MachineSocket::from_byte(byte[0]) else {
                send_status(control, libc::EINVAL as u8, None)?;
                continue;
            };
            match connect_under(root, socket.path()) {
                Ok(stream) => send_status(control, 0, Some(stream.as_fd()))?,
                Err(err) => send_status(control, errno_byte(&err), None)?,
            }
        }
    }

    /// Connect to `path` below `root` without needing access to the machine
    /// leader's procfs entry: `/proc/self/fd/<root>` is always our own.
    fn connect_under(root: BorrowedFd<'_>, path: &str) -> io::Result<UnixStream> {
        UnixStream::connect(format!("/proc/self/fd/{}{}", root.as_raw_fd(), path))
    }

    fn open_root(leader_pid: u32) -> io::Result<OwnedFd> {
        let root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
            .open(format!("/proc/{leader_pid}/root"))?;
        Ok(root.into())
    }

    /// Become unprivileged for good: no root identity, no capabilities, and
    /// no way to regain any.
    fn drop_privileges() -> io::Result<()> {
        // SAFETY: plain syscalls with valid arguments.
        unsafe {
            if libc::getuid() == 0 || libc::geteuid() == 0 {
                check(libc::setgroups(0, std::ptr::null()))?;
                check(libc::setresgid(NOBODY, NOBODY, NOBODY))?;
                check(libc::setresuid(NOBODY, NOBODY, NOBODY))?;
            }
            check(libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL,
                0,
                0,
                0,
            ))?;
            #[repr(C)]
            struct CapHeader {
                version: u32,
                pid: libc::c_int,
            }
            #[repr(C)]
            struct CapData {
                effective: u32,
                permitted: u32,
                inheritable: u32,
            }
            const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
            let header = CapHeader {
                version: LINUX_CAPABILITY_VERSION_3,
                pid: 0,
            };
            let data = [
                CapData {
                    effective: 0,
                    permitted: 0,
                    inheritable: 0,
                },
                CapData {
                    effective: 0,
                    permitted: 0,
                    inheritable: 0,
                },
            ];
            if libc::syscall(libc::SYS_capset, &header, data.as_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            check(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))?;
            // The helper lives inside the machine's PID namespace, where
            // processes running under the same numeric UID could otherwise
            // ptrace it (dropping from root already clears this flag).
            check(libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0))?;
        }
        Ok(())
    }

    fn check(ret: libc::c_int) -> io::Result<()> {
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn errno_byte(err: &io::Error) -> u8 {
        err.raw_os_error()
            .and_then(|errno| u8::try_from(errno).ok())
            .filter(|errno| *errno != 0)
            .unwrap_or(libc::EIO as u8)
    }

    pub(super) fn seqpacket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: fds has room for the two descriptors socketpair returns.
        check(unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        })?;
        // SAFETY: socketpair succeeded, so both are fresh fds we now own.
        Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
    }

    fn set_recv_timeout(fd: BorrowedFd<'_>, timeout: Duration) -> io::Result<()> {
        let tv = libc::timeval {
            tv_sec: timeout.as_secs().try_into().unwrap_or(libc::time_t::MAX),
            tv_usec: timeout.subsec_micros().into(),
        };
        // SAFETY: valid fd and a timeval that outlives the call.
        check(unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                std::ptr::from_ref(&tv).cast(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        })
    }

    /// Room for one fd worth of `SCM_RIGHTS`, suitably aligned.
    #[repr(C, align(8))]
    struct CmsgBuf([u8; 64]);

    /// Send a status byte, optionally with one fd attached.
    pub(super) fn send_status(
        sock: BorrowedFd<'_>,
        status: u8,
        fd: Option<BorrowedFd<'_>>,
    ) -> io::Result<()> {
        let mut data = [status];
        let mut iov = libc::iovec {
            iov_base: data.as_mut_ptr().cast(),
            iov_len: data.len(),
        };
        let mut cmsg_buf = CmsgBuf([0; 64]);
        // SAFETY: an all-zero msghdr is a valid empty message.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if let Some(fd) = fd {
            // SAFETY: CMSG_* on a buffer sized and aligned for one fd.
            unsafe {
                let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
                debug_assert!(space <= cmsg_buf.0.len());
                msg.msg_control = cmsg_buf.0.as_mut_ptr().cast();
                msg.msg_controllen = space as _;
                let cmsg = libc::CMSG_FIRSTHDR(&msg);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
                std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>(), fd.as_raw_fd());
            }
        }
        loop {
            // SAFETY: msg points at buffers that live until the call returns.
            if unsafe { libc::sendmsg(sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) } >= 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    /// Receive a status byte and the fd attached to it, if any.
    pub(super) fn recv_status(sock: BorrowedFd<'_>) -> io::Result<(u8, Option<OwnedFd>)> {
        let mut data = [0u8; 1];
        let mut iov = libc::iovec {
            iov_base: data.as_mut_ptr().cast(),
            iov_len: data.len(),
        };
        let mut cmsg_buf = CmsgBuf([0; 64]);
        // SAFETY: an all-zero msghdr is a valid empty message.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.0.as_mut_ptr().cast();
        msg.msg_controllen = cmsg_buf.0.len() as _;
        let n = loop {
            // SAFETY: msg points at buffers that live until the call returns.
            let n = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
            if n >= 0 {
                break n;
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        };
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "machine connector exited",
            ));
        }
        let mut received = None;
        // SAFETY: walking the control messages recvmsg filled in.
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let fd = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>());
                    received = Some(OwnedFd::from_raw_fd(fd));
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        if msg.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err(io::Error::other("truncated control message from connector"));
        }
        Ok((data[0], received))
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    use std::io;
    use std::os::fd::OwnedFd;
    use std::time::Duration;

    use super::{MachineConnector, MachineConnectorError, MachineSocket};

    pub(super) fn spawn(
        _leader_pid: u32,
        _timeout: Duration,
    ) -> Result<MachineConnector, MachineConnectorError> {
        Err(MachineConnectorError::Unsupported)
    }

    pub(super) fn request(
        _control: &OwnedFd,
        _socket: MachineSocket,
    ) -> io::Result<std::os::unix::net::UnixStream> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    pub(super) fn helper_run(_args: &[String]) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixListener;

    use super::sys::{recv_status, send_status, seqpacket_pair, serve};
    use super::*;

    #[test]
    fn test_machine_socket_byte_roundtrip() {
        for socket in MachineSocket::ALL {
            assert_eq!(MachineSocket::from_byte(socket as u8), Some(socket));
        }
        assert_eq!(MachineSocket::from_byte(0), None);
        assert_eq!(MachineSocket::from_byte(42), None);
    }

    #[test]
    fn test_machine_socket_paths() {
        assert_eq!(
            MachineSocket::Manager.path(),
            "/run/systemd/io.systemd.Manager"
        );
        assert_eq!(
            MachineSocket::Metrics.path(),
            "/run/systemd/report/io.systemd.Manager"
        );
        assert_eq!(
            MachineSocket::Network.path(),
            "/run/systemd/netif/io.systemd.Network"
        );
    }

    #[test]
    fn test_status_roundtrip_with_and_without_fd() {
        let (a, b) = seqpacket_pair().unwrap();
        send_status(a.as_fd(), 0, None).unwrap();
        let (status, fd) = recv_status(b.as_fd()).unwrap();
        assert_eq!((status, fd.is_some()), (0, false));

        let (mut x, mut y) = std::os::unix::net::UnixStream::pair().unwrap();
        send_status(a.as_fd(), 0, Some(x.as_fd())).unwrap();
        let (status, fd) = recv_status(b.as_fd()).unwrap();
        assert_eq!(status, 0);
        // The passed fd is the same socket: writing to it reaches y.
        let mut passed = std::os::unix::net::UnixStream::from(fd.unwrap());
        passed.write_all(b"hi").unwrap();
        let mut buf = [0u8; 2];
        y.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hi");
        x.flush().unwrap();

        send_status(a.as_fd(), libc::ENOENT as u8, None).unwrap();
        assert_eq!(recv_status(b.as_fd()).unwrap().0, libc::ENOENT as u8);

        drop(a);
        let err = recv_status(b.as_fd()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    /// The helper's request loop, against a temp dir standing in for a
    /// machine's root: allowlisted sockets connect, a missing one reports
    /// ENOENT, unknown requests EINVAL, and EOF ends the loop cleanly.
    #[test]
    fn test_serve_connects_below_root() {
        let root = tempfile::tempdir().unwrap();
        let manager = root.path().join("run/systemd/io.systemd.Manager");
        std::fs::create_dir_all(manager.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&manager).unwrap();
        let root_fd: OwnedFd = std::fs::File::open(root.path()).unwrap().into();

        let (control, helper_end) = seqpacket_pair().unwrap();
        let server = std::thread::spawn(move || serve(helper_end.as_fd(), root_fd.as_fd()));

        let stream = sys::request(&control, MachineSocket::Manager).unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        (&stream).write_all(b"ok").unwrap();
        let mut buf = [0u8; 2];
        accepted.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ok");

        let err = sys::request(&control, MachineSocket::Network).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));

        // SAFETY: plain send on a valid fd with a 1 byte buffer.
        let bogus = [99u8];
        assert_eq!(
            unsafe {
                libc::send(
                    std::os::fd::AsRawFd::as_raw_fd(&control),
                    bogus.as_ptr().cast(),
                    1,
                    0,
                )
            },
            1
        );
        assert_eq!(recv_status(control.as_fd()).unwrap().0, libc::EINVAL as u8);

        drop(control);
        server.join().unwrap().unwrap();
    }
}
