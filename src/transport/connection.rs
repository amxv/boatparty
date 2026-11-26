//! Own the dedicated SSH process that holds a serve or forward listener.
//! OpenSSH can keep forwarding channels alive after its session sees EOF.
use std::ffi::OsString;
use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};

#[cfg(unix)]
type Guard = io::PipeWriter;
#[cfg(windows)]
type Guard = win32job::Job;

/// The ordinary runner child, or a dedicated connection and its lifetime guard.
pub(crate) struct Connection {
    pub child: Child,
    guard: Option<Guard>,
}

impl Connection {
    pub fn spawn(mut command: Command, dedicated: bool, stderr: Stdio) -> io::Result<Self> {
        if dedicated {
            // Dedicated commands consist only of ssh and its arguments. The
            // helper inherits the same stdin, stdout and stderr as SSH.
            debug_assert_eq!(command.get_program(), "ssh");
            let mut helper = Command::new(std::env::current_exe()?);
            helper.args(["__ssh", "--"]);
            helper.args(command.get_args());
            command = helper;
        }
        #[cfg(unix)]
        let guard = if dedicated {
            use command_fds::{CommandFdExt, FdMapping};
            let (reader, writer) = io::pipe()?;
            command
                .fd_mappings(vec![FdMapping {
                    parent_fd: reader.into(),
                    child_fd: 3,
                }])
                .map_err(io::Error::other)?;
            Some(writer)
        } else {
            None
        };
        crate::system::separate_process_group(&mut command);
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()?;
        #[cfg(windows)]
        let mut child = child;
        #[cfg(windows)]
        let guard = if dedicated {
            match arm_job(&mut child) {
                Ok(job) => Some(job),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        } else {
            None
        };
        Ok(Self { child, guard })
    }

    pub fn kill(&mut self) -> io::Result<()> {
        if self.guard.take().is_some() {
            // EOF wakes the Unix supervisor. Closing the Windows Job Object
            // terminates its members. Neither path signals a saved PID.
            Ok(())
        } else {
            self.child.kill()
        }
    }
}

#[cfg(windows)]
fn arm_job(child: &mut Child) -> io::Result<win32job::Job> {
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;
    use win32job::{ExtendedLimitInfo, Job};
    let mut limits = ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job = Job::create_with_limit_info(&limits).map_err(io::Error::other)?;
    job.assign_process(child.as_raw_handle() as isize)
        .map_err(io::Error::other)?;
    // If the client dies before assignment, EOF makes the helper exit
    // without spawning SSH. After assignment both processes belong to us.
    child.stdin.as_mut().expect("piped stdin").write_all(b"G")?;
    Ok(job)
}

#[cfg(windows)]
pub(crate) fn supervise(args: &[OsString]) -> io::Result<u8> {
    {
        use std::io::Read;
        use std::os::windows::io::AsHandle;
        // Read exactly the permit byte, without buffering SSH's payload.
        let mut input = std::fs::File::from(io::stdin().as_handle().try_clone_to_owned()?);
        let mut permit = [0];
        if input.read(&mut permit)? != 1 || permit != *b"G" {
            return Ok(125);
        }
    }

    Ok(exit_code(Command::new("ssh").args(args).status()?))
}

#[cfg(unix)]
pub(crate) fn supervise(args: &[OsString]) -> io::Result<u8> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use signal_hook::consts::SIGCHLD;
    use std::io::Read;

    // FD 3 carries only liveness. Keep SSH stdin free for the runner
    // protocol, including graceful EOF while the client is still alive.
    let owner = std::fs::File::open("/dev/fd/3")?;
    let (mut changes, notify) = io::pipe()?;
    signal_hook::low_level::pipe::register(SIGCHLD, notify)?;
    if poll(
        &mut [PollFd::new(&owner, PollFlags::IN)],
        Some(&Timespec::default()),
    )? != 0
    {
        return Ok(125);
    }
    let mut ssh = Command::new("ssh").args(args).spawn()?;
    let mut wait = || -> io::Result<Option<ExitStatus>> {
        loop {
            if let Some(status) = ssh.try_wait()? {
                return Ok(Some(status));
            }
            let mut fds = [
                PollFd::new(&owner, PollFlags::IN),
                PollFd::new(&changes, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Err(rustix::io::Errno::INTR) => continue,
                result => result?,
            };
            if !fds[0].revents().is_empty() {
                return Ok(None);
            }
            let _ = changes.read(&mut [0; 64])?;
        }
    };
    match wait() {
        Ok(Some(status)) => Ok(exit_code(status)),
        result => {
            // This child has not been reaped. Its identity cannot be reused.
            let _ = ssh.kill();
            let status = ssh.wait()?;
            result.map(|_| exit_code(status))
        }
    }
}

fn exit_code(status: ExitStatus) -> u8 {
    status
        .code()
        .and_then(|code| code.try_into().ok())
        .unwrap_or(255)
}
