//! Observe and control an orphan without treating a recycled PID as ownership.
//!
//! This deliberately cannot reap the process or recover its exit status. The
//! caller retains the durable fence until the entire original process group is
//! absent, and performs the session transition under its own database claim.

use anyhow::{Context, Result};

use crate::application::{AGENT_PROJECT_ID_ENV, AGENT_RUN_TOKEN_ENV};

pub(crate) struct OrphanProcess {
    process_group: i32,
    project_id: i64,
    run_token: String,
    members: Vec<native::Process>,
}

impl OrphanProcess {
    /// `None` means the group is gone; a mismatch is an error, never ownership.
    /// No process is signaled by attachment or observation.
    pub(crate) fn attach(pid: u32, project_id: i64, run_token: &str) -> Result<Option<Self>> {
        let pid = i32::try_from(pid).context("Invalid orphan process PID")?;
        anyhow::ensure!(
            pid > 1 && project_id > 0 && !run_token.is_empty(),
            "Invalid orphan run identity"
        );
        let Some(anchor) = verified_anchor(pid, project_id, run_token)? else {
            if !super::agent_process_group_exists(pid)? {
                return Ok(None);
            }
            anyhow::bail!(
                "No live member of orphan group {pid} proves the exact automated run context"
            );
        };
        let mut orphan = Self {
            process_group: pid,
            project_id,
            run_token: run_token.to_owned(),
            members: vec![anchor],
        };
        orphan.refresh_members()?;
        Ok(Some(orphan))
    }

    /// A nonempty numeric group remains fenced, even if its leader has exited.
    /// Keeping zombies is intentional: filtering a snapshot after its live
    /// members fork and exit can incorrectly miss their newly born children.
    pub(crate) fn is_running(&mut self) -> Result<bool> {
        // Signal zero asks the kernel whether the whole group exists in one
        // operation; it delivers no signal. In particular, a /proc directory
        // walk can miss a child that forks while its parent exits mid-walk.
        super::agent_process_group_exists(self.process_group)
    }

    /// One nonblocking graceful shutdown sweep. Repeat while the group exists.
    pub(crate) fn stop(&mut self) -> Result<()> {
        self.signal(libc::SIGTERM)
    }

    /// One nonblocking escalation sweep, using the same exact identity checks.
    pub(crate) fn kill(&mut self) -> Result<()> {
        self.signal(libc::SIGKILL)
    }

    fn signal(&mut self, signal: i32) -> Result<()> {
        if !self.is_running()? {
            return Ok(());
        }
        self.refresh_members()?;
        for process in &self.members {
            if process.is_current()? {
                process.signal(signal)?;
            }
        }
        Ok(())
    }

    fn refresh_members(&mut self) -> Result<()> {
        // A process may spawn a successor and exit between monitoring ticks.
        // If cached handles no longer anchor the group, independently prove a
        // new exact-context member; never infer ownership from a reused PGID.
        for attempt in 0..2 {
            if attempt == 1 {
                let Some(anchor) =
                    verified_anchor(self.process_group, self.project_id, &self.run_token)?
                else {
                    break;
                };
                self.members = vec![anchor];
            }
            for anchor in &self.members {
                if !anchor.is_current()? {
                    continue;
                }
                let mut verified = Vec::new();
                for pid in native::group_members(self.process_group)? {
                    let Some(process) = native::Process::capture(pid)? else {
                        continue;
                    };
                    if process.process_group() != self.process_group {
                        continue;
                    }
                    let environment = process.environment();
                    if !process.is_current()? {
                        continue;
                    }
                    let environment = environment?;
                    if has_context(&environment) {
                        verify_context(&environment, self.project_id, &self.run_token)
                            .with_context(|| {
                                format!(
                                    "Cannot control unverified member {pid} of orphan group {}",
                                    self.process_group
                                )
                            })?;
                    }
                    verified.push(process);
                }
                // The SAME kernel generation must still be alive in the SAME
                // group at both ends of capture. This extends its exact group
                // ownership to redacted /bin/sh and other tool children without
                // trusting a bare numeric PGID. Retained member handles can anchor
                // another sweep even after the original leader exits.
                if anchor.is_current()? {
                    self.members = verified;
                    return Ok(());
                }
            }
        }
        anyhow::bail!(
            "No verified live process anchors orphan group {}",
            self.process_group
        )
    }
}

fn verified_anchor(
    process_group: i32,
    project_id: i64,
    run_token: &str,
) -> Result<Option<native::Process>> {
    for pid in native::group_members(process_group)? {
        let Some(process) = native::Process::capture(pid)? else {
            continue;
        };
        if process.process_group() != process_group {
            continue;
        }
        let environment = process.environment();
        if !process.is_current()? {
            continue;
        }
        let environment = environment?;
        if !has_context(&environment) {
            // macOS conceals platform-binary environments. Such a member can
            // only be adopted with an independently verified live anchor.
            continue;
        }
        verify_context(&environment, project_id, run_token)
            .with_context(|| format!("Cannot adopt orphan process {pid}"))?;
        process.check_signal_permission()?;
        if !process.is_current()? {
            continue;
        }
        return Ok(Some(process));
    }
    Ok(None)
}

fn has_context(environment: &[u8]) -> bool {
    environment.split(|byte| *byte == 0).any(|field| {
        field.starts_with(b"CLT_AGENT_PROJECT_ID=") || field.starts_with(b"CLT_AGENT_RUN_TOKEN=")
    })
}

fn verify_context(environment: &[u8], project_id: i64, run_token: &str) -> Result<()> {
    let project = format!("{AGENT_PROJECT_ID_ENV}={project_id}");
    let token = format!("{AGENT_RUN_TOKEN_ENV}={run_token}");
    let project_prefix = format!("{AGENT_PROJECT_ID_ENV}=");
    let token_prefix = format!("{AGENT_RUN_TOKEN_ENV}=");
    let fields = environment.split(|byte| *byte == 0).collect::<Vec<_>>();
    // Reject duplicates as well as wrong values; environment inspection must
    // not choose a different value from the one the process actually consumes.
    anyhow::ensure!(
        fields
            .iter()
            .filter(|field| field.starts_with(project_prefix.as_bytes()))
            .copied()
            .eq([project.as_bytes()])
            && fields
                .iter()
                .filter(|field| field.starts_with(token_prefix.as_bytes()))
                .copied()
                .eq([token.as_bytes()]),
        "Process does not carry the exact automated project and run context"
    );
    Ok(())
}

#[cfg(target_os = "macos")]
mod native {
    use std::{io, mem, ptr, sync::OnceLock};

    use anyhow::{Context, Result};

    // Darwin's stable 56-byte proc_uniqidentifierinfo ABI. Its declaration is
    // in Apple's private header, although proc_pidinfo is a public libproc API.
    // The former reserved fields have changed names; the PID version remains
    // at byte 32. Unsupported kernels/errors fail closed, never fall back to
    // check-then-kill(pid).
    // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info_private.h
    const PROC_PIDUNIQIDENTIFIERINFO: i32 = 17;
    #[repr(C)]
    #[derive(Clone, Copy, Default, PartialEq)]
    struct UniqueInfo {
        executable_uuid: [u8; 16],
        unique_id: u64,
        parent_unique_id: u64,
        pid_version: i32,
        reserved: [u32; 5],
    }
    const _: () = assert!(mem::size_of::<UniqueInfo>() == 56);

    #[repr(C)]
    struct AuditToken {
        values: [u32; 8],
    }

    type SignalFunction = unsafe extern "C" fn(*mut AuditToken, i32) -> i32;

    fn signal_function() -> Result<SignalFunction> {
        // Public SDK libproc.h. Unlike kill(pid), XNU checks PID+pidversion
        // while holding a kernel process reference through signal delivery.
        // Resolve at runtime so older macOS versions can still run CLT while
        // declining adoption if they do not provide this safety primitive.
        // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c
        static FUNCTION: OnceLock<Option<SignalFunction>> = OnceLock::new();
        FUNCTION
            .get_or_init(|| {
                let symbol = unsafe {
                    libc::dlsym(libc::RTLD_DEFAULT, c"proc_signal_with_audittoken".as_ptr())
                };
                if symbol.is_null() {
                    None
                } else {
                    Some(unsafe { mem::transmute::<*mut libc::c_void, SignalFunction>(symbol) })
                }
            })
            .context("This macOS version cannot safely control orphan process generations")
    }

    pub(super) struct Process {
        pid: i32,
        process_group: i32,
        identity: UniqueInfo,
    }

    impl Process {
        pub(super) fn capture(pid: i32) -> Result<Option<Self>> {
            signal_function()?;
            let Some(identity) = identity(pid)? else {
                return Ok(None);
            };
            let mut bsd = unsafe { mem::zeroed::<libc::proc_bsdinfo>() };
            let bytes = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    ptr::from_mut(&mut bsd).cast(),
                    mem::size_of_val(&bsd) as i32,
                )
            };
            if bytes <= 0 {
                return gone_or_error("Cannot inspect orphan process group");
            }
            anyhow::ensure!(
                bytes as usize == mem::size_of_val(&bsd),
                "Incomplete orphan process information"
            );
            if bsd.pbi_status == libc::SZOMB {
                return Ok(None);
            }
            let process = Self {
                pid,
                process_group: bsd.pbi_pgid as i32,
                identity,
            };
            if process.is_current()? {
                Ok(Some(process))
            } else {
                Ok(None)
            }
        }

        pub(super) fn process_group(&self) -> i32 {
            self.process_group
        }

        pub(super) fn is_current(&self) -> Result<bool> {
            let matches = |now: UniqueInfo| {
                now.unique_id == self.identity.unique_id
                    && now.pid_version == self.identity.pid_version
            };
            if !identity(self.pid)?.is_some_and(matches) {
                return Ok(false);
            }
            // Sandwich the numeric getpgid observation between exact kernel
            // generation reads, so it cannot describe a replacement process.
            let group = unsafe { libc::getpgid(self.pid) };
            if group < 0 {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(false)
                } else {
                    Err(error).context("Cannot verify orphan process-group anchor")
                };
            }
            Ok(group == self.process_group && identity(self.pid)?.is_some_and(matches))
        }

        pub(super) fn environment(&self) -> Result<Vec<u8>> {
            let mut argmax = 0_i32;
            let mut size = mem::size_of_val(&argmax);
            let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
            let result = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    2,
                    ptr::from_mut(&mut argmax).cast(),
                    &mut size,
                    ptr::null_mut(),
                    0,
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error())
                    .context("Cannot determine process argument buffer size");
            }
            anyhow::ensure!(
                argmax > 0 && argmax <= 16 * 1024 * 1024,
                "Invalid process argument buffer size"
            );
            let mut buffer = vec![0_u8; argmax as usize];
            size = buffer.len();
            let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, self.pid];
            let result = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    3,
                    buffer.as_mut_ptr().cast(),
                    &mut size,
                    ptr::null_mut(),
                    0,
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error())
                    .context("Cannot inspect orphan run environment");
            }
            buffer.truncate(size);
            parse_environment(&buffer)
        }

        pub(super) fn signal(&self, signal: i32) -> Result<()> {
            let mut token = AuditToken { values: [0; 8] };
            // XNU uses the target token's PID and version for lookup; sender
            // privileges are checked against the actual calling credentials.
            token.values[5] = self.pid as u32;
            token.values[7] = self.identity.pid_version as u32;
            let error = unsafe { signal_function()?(&mut token, signal) };
            match error {
                0 | libc::ESRCH => Ok(()),
                _ => Err(io::Error::from_raw_os_error(error))
                    .with_context(|| format!("Cannot signal verified orphan process {}", self.pid)),
            }
        }

        pub(super) fn check_signal_permission(&self) -> Result<()> {
            // Darwin's audit-token API rejects signal zero. Numeric signal
            // zero has no side effects; the caller sandwiches this permission
            // observation with exact generation/group checks. The real signal
            // is still exclusively delivered by audit token.
            let result = unsafe { libc::kill(self.pid, 0) };
            if result == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            Err(error).context("Cannot control verified orphan process")
        }
    }

    fn identity(pid: i32) -> Result<Option<UniqueInfo>> {
        let mut info = UniqueInfo::default();
        let bytes = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDUNIQIDENTIFIERINFO,
                0,
                ptr::from_mut(&mut info).cast(),
                mem::size_of_val(&info) as i32,
            )
        };
        if bytes <= 0 {
            return gone_or_error("Cannot obtain orphan process generation");
        }
        anyhow::ensure!(
            bytes as usize == mem::size_of_val(&info) && info.unique_id != 0,
            "Incomplete orphan process generation"
        );
        Ok(Some(info))
    }

    fn gone_or_error<T>(context: &str) -> Result<Option<T>> {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(None)
        } else {
            Err(error).context(context.to_owned())
        }
    }

    pub(super) fn group_members(process_group: i32) -> Result<Vec<i32>> {
        const PROC_PGRP_ONLY: u32 = 2;
        let mut members = vec![0_i32; 64];
        loop {
            let size = mem::size_of_val(members.as_slice());
            let bytes = unsafe {
                // proc_listpids returns BYTES; the convenient
                // proc_listpgrppids wrapper instead returns a PID COUNT.
                libc::proc_listpids(
                    PROC_PGRP_ONLY,
                    process_group as u32,
                    members.as_mut_ptr().cast(),
                    size as i32,
                )
            };
            anyhow::ensure!(
                bytes >= 0,
                "Cannot enumerate orphan process group: {}",
                io::Error::last_os_error()
            );
            anyhow::ensure!(
                (bytes as usize).is_multiple_of(mem::size_of::<i32>()),
                "Incomplete orphan process-group membership"
            );
            if (bytes as usize) < size {
                members.truncate(bytes as usize / mem::size_of::<i32>());
                members.retain(|pid| *pid > 0);
                return Ok(members);
            }
            anyhow::ensure!(
                members.len() < 1_048_576,
                "Orphan process-group membership exceeds inspection limit"
            );
            members.resize(members.len() * 2, 0);
        }
    }

    fn parse_environment(buffer: &[u8]) -> Result<Vec<u8>> {
        let argc_bytes: [u8; 4] = buffer
            .get(..4)
            .context("Truncated orphan process arguments")?
            .try_into()?;
        let argc = i32::from_ne_bytes(argc_bytes);
        anyhow::ensure!(argc > 0, "Invalid orphan process argument count");
        let mut offset = 4;
        skip_string(buffer, &mut offset)?; // executable path
        while buffer.get(offset) == Some(&0) {
            offset += 1;
        }
        for _ in 0..argc {
            skip_string(buffer, &mut offset)?;
        }
        Ok(buffer[offset..].to_vec())
    }

    fn skip_string(buffer: &[u8], offset: &mut usize) -> Result<()> {
        let end = buffer
            .get(*offset..)
            .and_then(|rest| rest.iter().position(|byte| *byte == 0))
            .context("Truncated orphan process argument string")?;
        *offset += end + 1;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{Process, parse_environment};

        #[test]
        fn arguments_are_not_mistaken_for_environment() {
            let mut buffer = 3_i32.to_ne_bytes().to_vec();
            buffer.extend_from_slice(b"/bin/codex\0\0codex\0CLT_AGENT_RUN_TOKEN=wrong\0\0CLT_AGENT_PROJECT_ID=42\0CLT_AGENT_RUN_TOKEN=right\0\0");
            assert_eq!(
                parse_environment(&buffer).unwrap(),
                b"CLT_AGENT_PROJECT_ID=42\0CLT_AGENT_RUN_TOKEN=right\0\0"
            );
            assert!(parse_environment(&buffer[..8]).is_err());
        }

        #[test]
        fn wrong_generation_cannot_receive_signal() {
            let mut child = std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap();
            let mut process = Process::capture(child.id() as i32).unwrap().unwrap();
            process.identity.pid_version = process.identity.pid_version.wrapping_add(1);
            // A wrong-generation SIGKILL must be rejected by the kernel even
            // though that numeric PID belongs to a live, signalable child.
            process.signal(libc::SIGKILL).unwrap();
            assert!(child.try_wait().unwrap().is_none());
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
}

#[cfg(target_os = "linux")]
mod native {
    use std::{
        ffi::CStr,
        fs::{self, File},
        io::{self, Read},
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        ptr,
    };

    use anyhow::{Context, Result};

    pub(super) struct Process {
        pid: i32,
        process_group: i32,
        pidfd: OwnedFd,
        proc_dir: File,
    }

    impl Process {
        pub(super) fn capture(pid: i32) -> Result<Option<Self>> {
            // Acquire the kernel handle before opening /proc. If PID reuse
            // races either operation, the original handle becomes readable
            // and is_current rejects the acquisition before any signaling.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
            if fd < 0 {
                return gone_or_error("Cannot obtain orphan pidfd");
            }
            let pidfd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
            let proc_dir = match File::open(format!("/proc/{pid}")) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).context("Cannot open orphan process directory"),
            };
            let mut process = Self {
                pid,
                process_group: 0,
                pidfd,
                proc_dir,
            };
            if !process.is_current()? {
                return Ok(None);
            }
            let stat = process.read(c"stat")?;
            let (group, state) = parse_stat(&stat)?;
            if state == b'Z' || state == b'X' {
                return Ok(None);
            }
            process.process_group = group;
            if process.is_current()? {
                Ok(Some(process))
            } else {
                Ok(None)
            }
        }

        pub(super) fn process_group(&self) -> i32 {
            self.process_group
        }

        pub(super) fn is_current(&self) -> Result<bool> {
            if !self.handle_is_alive()? {
                return Ok(false);
            }
            if self.process_group != 0 {
                let stat = self.read(c"stat");
                if !self.handle_is_alive()? {
                    return Ok(false);
                }
                let (group, state) = parse_stat(&stat?)?;
                if group != self.process_group || state == b'Z' || state == b'X' {
                    return Ok(false);
                }
            }
            self.handle_is_alive()
        }

        fn handle_is_alive(&self) -> Result<bool> {
            let mut fd = libc::pollfd {
                fd: self.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut fd, 1, 0) };
            if result < 0 {
                return Err(io::Error::last_os_error()).context("Cannot inspect orphan pidfd");
            }
            anyhow::ensure!(fd.revents & libc::POLLNVAL == 0, "Invalid orphan pidfd");
            Ok(result == 0)
        }

        pub(super) fn environment(&self) -> Result<Vec<u8>> {
            self.read(c"environ")
        }

        fn read(&self, name: &CStr) -> Result<Vec<u8>> {
            let fd = unsafe {
                libc::openat(
                    self.proc_dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error())
                    .context("Cannot read orphan process identity");
            }
            let mut file = unsafe { File::from_raw_fd(fd) };
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .context("Cannot read orphan process identity")?;
            Ok(bytes)
        }

        pub(super) fn signal(&self, signal: i32) -> Result<()> {
            // Linux 5.3+; an unavailable syscall fails closed. Always signal
            // individual pidfds, never a numeric PID/PGID after a userspace
            // identity check. https://man7.org/linux/man-pages/man2/pidfd_send_signal.2.html
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.pidfd.as_raw_fd(),
                    signal,
                    ptr::null::<libc::siginfo_t>(),
                    0_u32,
                )
            };
            if result == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            Err(error)
                .with_context(|| format!("Cannot signal verified orphan process {}", self.pid))
        }

        pub(super) fn check_signal_permission(&self) -> Result<()> {
            self.signal(0)
        }
    }

    fn gone_or_error<T>(context: &str) -> Result<Option<T>> {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(None)
        } else {
            Err(error).context(context.to_owned())
        }
    }

    pub(super) fn group_members(process_group: i32) -> Result<Vec<i32>> {
        let mut members = Vec::new();
        for entry in fs::read_dir("/proc").context("Cannot enumerate orphan process group")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            let stat = match fs::read(entry.path().join("stat")) {
                Ok(stat) => stat,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("Cannot inspect process-group membership"),
            };
            if parse_stat(&stat)?.0 == process_group {
                members.push(pid);
            }
        }
        Ok(members)
    }

    fn parse_stat(bytes: &[u8]) -> Result<(i32, u8)> {
        // comm can contain spaces and closing parentheses. The last ')' ends
        // it; fields after it are state (3), ppid (4), process group (5).
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b')')
            .context("Malformed process stat")?;
        let text = std::str::from_utf8(&bytes[end + 1..]).context("Malformed process stat")?;
        let fields = text.split_ascii_whitespace().take(3).collect::<Vec<_>>();
        anyhow::ensure!(
            fields.len() == 3 && fields[0].len() == 1,
            "Incomplete process stat"
        );
        Ok((
            fields[2].parse().context("Invalid process-group stat")?,
            fields[0].as_bytes()[0],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{AGENT_PROJECT_ID_ENV, AGENT_RUN_TOKEN_ENV, OrphanProcess, verify_context};
    use std::{
        os::unix::process::CommandExt,
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    struct Fixture(Child);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn fixture(project_id: i64, token: &str) -> Fixture {
        Fixture(
            fixture_command()
                .env(AGENT_PROJECT_ID_ENV, project_id.to_string())
                .env(AGENT_RUN_TOKEN_ENV, token)
                .process_group(0)
                .spawn()
                .unwrap(),
        )
    }

    fn fixture_command() -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("platform::orphan::tests::orphan_process_fixture_entry")
            .env("CLT_TEST_ORPHAN_FIXTURE", "sleep")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    #[test]
    #[allow(clippy::zombie_processes)] // The detach fixture deliberately outlives its parent.
    fn orphan_process_fixture_entry() {
        match std::env::var("CLT_TEST_ORPHAN_FIXTURE").as_deref() {
            Ok("sleep") => thread::sleep(Duration::from_secs(30)),
            Ok("detach") => {
                let child = fixture_command().process_group(0).spawn().unwrap();
                std::fs::write(
                    std::env::var_os("CLT_TEST_ORPHAN_PID_FILE").unwrap(),
                    child.id().to_string(),
                )
                .unwrap();
                // Returning exits the launch helper; its live child is adopted
                // by init/launchd before the supervising test attaches.
            }
            _ => {}
        }
    }

    #[test]
    fn context_requires_exact_values_and_rejects_duplicates() {
        assert!(
            verify_context(
                b"CLT_AGENT_PROJECT_ID=42\0CLT_AGENT_RUN_TOKEN=right\0",
                42,
                "right"
            )
            .is_ok()
        );
        assert!(
            verify_context(
                b"CLT_AGENT_PROJECT_ID=42\0CLT_AGENT_RUN_TOKEN=right-extra\0",
                42,
                "right"
            )
            .is_err()
        );
        assert!(
            verify_context(
                b"CLT_AGENT_PROJECT_ID=42\0CLT_AGENT_RUN_TOKEN=right\0CLT_AGENT_RUN_TOKEN=wrong\0",
                42,
                "right"
            )
            .is_err()
        );
    }

    #[test]
    fn attach_preserves_work_and_rejects_other_run_and_project() {
        let mut child = fixture(42, "orphan-context-test");
        let mut orphan = OrphanProcess::attach(child.0.id(), 42, "orphan-context-test")
            .unwrap()
            .unwrap();
        assert!(orphan.is_running().unwrap());
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(OrphanProcess::attach(child.0.id(), 43, "orphan-context-test").is_err());
        assert!(OrphanProcess::attach(child.0.id(), 42, "wrong-token").is_err());
        assert!(child.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn stop_verified_process_and_observe_group_exit() {
        let mut child = fixture(42, "orphan-stop-test");
        let mut orphan = OrphanProcess::attach(child.0.id(), 42, "orphan-stop-test")
            .unwrap()
            .unwrap();
        orphan.stop().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "Verified orphan did not stop");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!orphan.is_running().unwrap());
        assert!(
            OrphanProcess::attach(child.0.id(), 42, "orphan-stop-test")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unverified_group_member_prevents_signaling_every_member() {
        let mut leader = fixture(42, "orphan-foreign-member-test");
        let mut orphan = OrphanProcess::attach(leader.0.id(), 42, "orphan-foreign-member-test")
            .unwrap()
            .unwrap();
        let mut foreign = Fixture(
            fixture_command()
                .env(AGENT_PROJECT_ID_ENV, "42")
                .env(AGENT_RUN_TOKEN_ENV, "a-different-run")
                .process_group(leader.0.id() as i32)
                .spawn()
                .unwrap(),
        );
        assert!(orphan.stop().is_err());
        assert!(orphan.kill().is_err());
        assert!(leader.0.try_wait().unwrap().is_none());
        assert!(foreign.0.try_wait().unwrap().is_none());
        assert!(orphan.is_running().unwrap());
    }

    #[test]
    fn cached_group_member_remains_controllable_after_leader_exit() {
        let mut leader = fixture(42, "orphan-cached-tools-test");
        let mut tool = Fixture(
            Command::new("/bin/sleep")
                .arg("30")
                .env(AGENT_PROJECT_ID_ENV, "42")
                .env(AGENT_RUN_TOKEN_ENV, "orphan-cached-tools-test")
                .process_group(leader.0.id() as i32)
                .spawn()
                .unwrap(),
        );
        let mut orphan = OrphanProcess::attach(leader.0.id(), 42, "orphan-cached-tools-test")
            .unwrap()
            .unwrap();
        leader.0.kill().unwrap();
        leader.0.wait().unwrap();
        assert!(orphan.is_running().unwrap());
        orphan.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tool.0.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "Cached tool process did not stop"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!orphan.is_running().unwrap());
    }

    #[test]
    fn attaches_to_verified_member_after_group_leader_exits() {
        let mut leader = fixture(42, "orphan-leaderless-test");
        let mut member = Fixture(
            fixture_command()
                .env(AGENT_PROJECT_ID_ENV, "42")
                .env(AGENT_RUN_TOKEN_ENV, "orphan-leaderless-test")
                .process_group(leader.0.id() as i32)
                .spawn()
                .unwrap(),
        );
        let group = leader.0.id();
        leader.0.kill().unwrap();
        leader.0.wait().unwrap();
        let mut orphan = OrphanProcess::attach(group, 42, "orphan-leaderless-test")
            .unwrap()
            .unwrap();
        orphan.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while member.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "Leaderless process did not stop");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!orphan.is_running().unwrap());
    }

    #[test]
    fn adopts_and_stops_a_process_after_its_actual_parent_exits() {
        let pid_file =
            std::env::temp_dir().join(format!("clt-orphan-platform-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&pid_file);
        let status = fixture_command()
            .env("CLT_TEST_ORPHAN_FIXTURE", "detach")
            .env("CLT_TEST_ORPHAN_PID_FILE", &pid_file)
            .env(AGENT_PROJECT_ID_ENV, "42")
            .env(AGENT_RUN_TOKEN_ENV, "orphan-reparented-test")
            .status()
            .unwrap();
        assert!(status.success());
        let pid = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        std::fs::remove_file(pid_file).unwrap();
        let mut orphan = OrphanProcess::attach(pid, 42, "orphan-reparented-test")
            .unwrap()
            .unwrap();
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        assert!(orphan.is_running().unwrap());
        orphan.stop().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while orphan.is_running().unwrap() {
            assert!(Instant::now() < deadline, "Reparented process did not stop");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn discovers_exact_successor_when_every_cached_anchor_has_exited() {
        let mut leader = fixture(42, "orphan-successor-test");
        let mut orphan = OrphanProcess::attach(leader.0.id(), 42, "orphan-successor-test")
            .unwrap()
            .unwrap();
        let mut successor = Fixture(
            fixture_command()
                .env(AGENT_PROJECT_ID_ENV, "42")
                .env(AGENT_RUN_TOKEN_ENV, "orphan-successor-test")
                .process_group(leader.0.id() as i32)
                .spawn()
                .unwrap(),
        );
        leader.0.kill().unwrap();
        leader.0.wait().unwrap();
        orphan.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while successor.0.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "New verified successor did not stop"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!orphan.is_running().unwrap());
    }
}
