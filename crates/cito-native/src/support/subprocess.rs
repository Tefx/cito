//! Deadline-aware pipe draining and owned subprocess-group cleanup.
use super::{Duration, File, Result};

/// Observe an owned, unreaped child without releasing its PID before cleanup.
pub fn child_exit_fd(pid: u32) -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    #[cfg(target_os = "macos")]
    {
        // SAFETY: kqueue returns a new descriptor and kevent borrows initialized data.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        let change = libc::kevent {
            ident: pid as _,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if unsafe { libc::kevent(fd, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(owned)
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: pidfd_open observes a child whose PID remains reserved until wait.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
    }
}

/// Block for descriptor readiness, preserving the absolute deadline across signals.
pub fn poll_until(fds: &mut [libc::pollfd], deadline: Option<std::time::Instant>) -> Result<bool> {
    loop {
        let timeout = deadline
            .map(|at| {
                at.saturating_duration_since(std::time::Instant::now())
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32
            })
            .unwrap_or(-1);
        // SAFETY: poll borrows a live initialized slice for the duration of the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) };
        if n >= 0 {
            return Ok(n > 0);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.to_string());
        }
        if deadline.is_some_and(|at| at <= std::time::Instant::now()) {
            return Ok(false);
        }
    }
}

pub fn wait_child_exit(child: &std::process::Child, timeout: Duration) -> Result<bool> {
    use std::os::fd::AsRawFd;
    let fd = child_exit_fd(child.id())?;
    poll_until(
        &mut [libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }],
        Some(std::time::Instant::now() + timeout),
    )
}

pub fn run_command_with_timeout(
    command: std::process::Command,
    timeout: Duration,
) -> Result<Option<std::process::Output>> {
    run_piped_command(command, None, timeout, [usize::MAX; 2], None)
}

/// Drain input/output concurrently under one deadline. Progress checks retain their
/// caller-owned fencing semantics; ordinary subprocess callers need no timer ticks.
pub fn run_piped_command(
    mut command: std::process::Command,
    input: Option<&[u8]>,
    timeout: Duration,
    limits: [usize; 2],
    mut progress: Option<&mut dyn FnMut(Duration) -> Result<()>>,
) -> Result<Option<std::process::Output>> {
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    let started = std::time::Instant::now();
    let deadline = started + timeout;
    let mut child = command
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let result = (|| -> Result<Option<[Vec<u8>; 2]>> {
        let mut stdin = child.stdin.take();
        if input.is_none() {
            stdin.take();
        }
        if input.is_some() && stdin.is_none() {
            return Err("Missing subprocess stdin".into());
        }
        let input = input.unwrap_or_default();
        let mut written = 0;
        let mut pipes = [
            child.stdout.take().map(|p| File::from(OwnedFd::from(p))),
            child.stderr.take().map(|p| File::from(OwnedFd::from(p))),
        ];
        for fd in stdin
            .iter()
            .map(AsRawFd::as_raw_fd)
            .chain(pipes.iter().flatten().map(AsRawFd::as_raw_fd))
        {
            // SAFETY: these owned pipes remain live through the drain.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
        }
        let exited = child_exit_fd(child.id())?;
        let mut child_exited = false;
        let mut output = [Vec::new(), Vec::new()];
        loop {
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            if let Some(progress) = progress.as_mut() {
                progress(started.elapsed())?;
            }
            if let Some(pipe) = stdin.as_mut() {
                if written < input.len() {
                    match pipe.write(&input[written..]) {
                        Ok(0) => return Err("Child closed stdin before input completed".into()),
                        Ok(n) => written += n,
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e.to_string()),
                    }
                }
                if written == input.len() {
                    stdin.take();
                }
            }
            for ((pipe, buffer), limit) in pipes.iter_mut().zip(&mut output).zip(limits) {
                if let Some(reader) = pipe {
                    let mut bytes = [0u8; 8192];
                    // Read one chunk per stream so a busy writer cannot starve the
                    // deadline, input writes, peer stream, or progress callback.
                    match reader.read(&mut bytes) {
                        Ok(0) => *pipe = None,
                        Ok(n) => {
                            if n > limit.saturating_sub(buffer.len()) {
                                return Err("Output exceeds limit".into());
                            }
                            buffer.extend_from_slice(&bytes[..n]);
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e.to_string()),
                    }
                }
            }
            if child_exited && pipes.iter().all(Option::is_none) {
                return Ok(Some(output));
            }
            let mut fds = [
                libc::pollfd {
                    fd: stdin.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    events: libc::POLLOUT,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pipes[0].as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pipes[1].as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if child_exited { -1 } else { exited.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let wake_at = if progress.is_some() {
                deadline.min(std::time::Instant::now() + Duration::from_millis(250))
            } else {
                deadline
            };
            poll_until(&mut fds, Some(wake_at))?;
            if !child_exited && fds[3].revents != 0 {
                child_exited = true;
                // SAFETY: the unreaped child reserves its owned process-group ID.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
            }
        }
    })();
    if !matches!(result, Ok(Some(_))) {
        // SAFETY: the child remains unreaped until wait, reserving the group ID.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGTERM);
        }
        let _ = wait_child_exit(&child, Duration::from_millis(50));
    }
    // Cleanup background grandchildren even after a successful parent exit.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let status = child.wait().map_err(|e| e.to_string());
    let Some([stdout, stderr]) = result? else {
        return Ok(None);
    };
    Ok(Some(std::process::Output {
        status: status?,
        stdout,
        stderr,
    }))
}
