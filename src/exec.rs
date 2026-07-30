use crate::fs_safety;
use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use nix::unistd::{Uid, User};
use std::ffi::CString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CHILD_ERROR_FD: RawFd = 3;

#[derive(Debug, Clone)]
pub struct BuildUser {
    pub name: String,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    pub home: Utf8PathBuf,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandResult {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Utf8PathBuf,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Debug)]
pub struct UserCommand<'a> {
    pub user: &'a BuildUser,
    pub program: &'a str,
    pub args: Vec<String>,
    pub cwd: &'a Utf8Path,
    pub stdout_file: Option<&'a Utf8Path>,
    pub stderr_file: Option<&'a Utf8Path>,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ChildExecError {
    step: u8,
    _padding: [u8; 3],
    errno: i32,
}

struct PreparedExec {
    path: CString,
    cwd: CString,
    stdout_file: Option<CString>,
    stderr_file: Option<CString>,
    argv: Vec<CString>,
    env: Vec<CString>,
    argv_ptrs: Vec<*const libc::c_char>,
    env_ptrs: Vec<*const libc::c_char>,
}

impl PreparedExec {
    fn new(spec: &UserCommand<'_>) -> Result<Self> {
        let program_path = resolve_program(spec.program);
        let path = cstring_path(&program_path)?;
        let cwd = CString::new(spec.cwd.as_os_str().as_bytes())
            .map_err(|_| anyhow::anyhow!("command cwd contains NUL: {}", spec.cwd))?;
        let stdout_file = spec
            .stdout_file
            .map(|path| {
                CString::new(path.as_os_str().as_bytes())
                    .map_err(|_| anyhow::anyhow!("stdout path contains NUL: {path}"))
            })
            .transpose()?;
        let stderr_file = spec
            .stderr_file
            .map(|path| {
                CString::new(path.as_os_str().as_bytes())
                    .map_err(|_| anyhow::anyhow!("stderr path contains NUL: {path}"))
            })
            .transpose()?;

        let mut argv = Vec::with_capacity(spec.args.len() + 1);
        argv.push(CString::new(spec.program.as_bytes())?);
        for arg in &spec.args {
            argv.push(
                CString::new(arg.as_bytes())
                    .map_err(|_| anyhow::anyhow!("command argument contains NUL: {arg:?}"))?,
            );
        }

        let mut env = vec![
            CString::new(format!("HOME={}", spec.user.home))?,
            CString::new(format!("USER={}", spec.user.name))?,
            CString::new(format!("LOGNAME={}", spec.user.name))?,
            CString::new("GIT_TERMINAL_PROMPT=0")?,
            CString::new("GIT_ASKPASS=/usr/bin/false")?,
            CString::new(format!(
                "PATH=/usr/local/bin:/usr/bin:/bin:{}/.local/bin",
                spec.user.home
            ))?,
        ];
        copy_host_locale_env(&mut env)?;

        let mut exec = Self {
            path,
            cwd,
            stdout_file,
            stderr_file,
            argv,
            env,
            argv_ptrs: Vec::new(),
            env_ptrs: Vec::new(),
        };
        exec.refresh_ptrs();
        Ok(exec)
    }

    fn refresh_ptrs(&mut self) {
        self.argv_ptrs = self.argv.iter().map(|arg| arg.as_ptr()).collect();
        self.argv_ptrs.push(std::ptr::null());
        self.env_ptrs = self.env.iter().map(|item| item.as_ptr()).collect();
        self.env_ptrs.push(std::ptr::null());
    }
}

pub fn lookup_build_user(user_name: &str) -> Result<BuildUser> {
    let user = User::from_name(user_name)
        .with_context(|| format!("failed to look up user {user_name}"))?
        .ok_or_else(|| anyhow::anyhow!("user {user_name} does not exist"))?;
    if user.uid == Uid::from_raw(0) {
        bail!("refusing to use root as the AUR build user");
    }
    let home = Utf8PathBuf::from_path_buf(user.dir)
        .map_err(|path| anyhow::anyhow!("build user home path is not UTF-8: {path:?}"))?;
    Ok(BuildUser {
        name: user.name,
        uid: user.uid.as_raw(),
        gid: user.gid.as_raw(),
        home,
    })
}

pub fn ensure_user_owned_dir(path: &Utf8Path, user: &BuildUser) -> Result<()> {
    let directory = fs_safety::ensure_directory_nofollow(path)
        .with_context(|| format!("failed to create build directory {path} safely"))?;
    // SAFETY: directory is an owned descriptor opened without following symlinks.
    if unsafe { libc::fchown(directory.as_raw_fd(), user.uid, user.gid) } < 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("failed to chown {path} to {}", user.name));
    }
    // SAFETY: directory is an owned descriptor for the build directory.
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o755) } < 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("failed to chmod build directory {path}"));
    }
    Ok(())
}

pub fn run_as_user(spec: UserCommand<'_>) -> Result<CommandResult> {
    let mut exec = PreparedExec::new(&spec)?;
    let (mut error_read, error_write) = pipe_cloexec()?;
    let mut pidfd: libc::c_int = -1;
    let mut args = libc::clone_args {
        flags: u64::try_from(libc::CLONE_PIDFD).unwrap_or(0),
        pidfd: (&mut pidfd as *mut libc::c_int) as u64,
        child_tid: 0,
        parent_tid: 0,
        exit_signal: u64::try_from(libc::SIGCHLD).unwrap_or(0),
        stack: 0,
        stack_size: 0,
        tls: 0,
        set_tid: 0,
        set_tid_size: 0,
        cgroup: 0,
    };

    // SAFETY: clone3 is called without CLONE_VM, giving the child a fork-like
    // address space. The child path uses direct libc syscalls and exits on any
    // setup failure before execve.
    let raw_pid = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &mut args as *mut libc::clone_args,
            std::mem::size_of::<libc::clone_args>(),
        )
    };
    if raw_pid < 0 {
        let error = io::Error::last_os_error();
        bail!("clone3 failed for {}: {error}", spec.program);
    }

    if raw_pid == 0 {
        drop(error_read);
        child_exec_or_exit(
            &mut exec,
            spec.user.uid,
            spec.user.gid,
            error_write.as_raw_fd(),
        );
    }

    drop(error_write);
    if pidfd < 0 {
        bail!("clone3 did not return a pidfd for {}", spec.program);
    }
    // SAFETY: clone3 wrote a fresh pidfd owned by this parent process.
    let _pidfd = unsafe { OwnedFd::from_raw_fd(pidfd) };
    let child_pid = libc::pid_t::try_from(raw_pid)
        .map_err(|_| anyhow::anyhow!("invalid child pid {raw_pid}"))?;
    read_child_exec_status(&mut error_read, child_pid, spec.program)?;
    let status = wait_for_child(child_pid)?;
    let result = CommandResult {
        program: spec.program.to_owned(),
        args: spec.args,
        cwd: spec.cwd.to_owned(),
        exit_code: exit_code(status),
        signal: term_signal(status),
    };
    if result.exit_code != Some(0) {
        bail!(
            "{} failed with exit_code={:?} signal={:?}",
            result.program,
            result.exit_code,
            result.signal
        );
    }
    Ok(result)
}

pub fn run_as_user_capture(
    user: &BuildUser,
    program: &str,
    args: Vec<String>,
    cwd: &Utf8Path,
) -> Result<String> {
    if Uid::effective().as_raw() == user.uid {
        let output = Command::new(resolve_program(program))
            .args(&args)
            .current_dir(cwd)
            .env_clear()
            .env("HOME", user.home.as_str())
            .env("USER", &user.name)
            .env("LOGNAME", &user.name)
            .env(
                "PATH",
                format!("/usr/local/bin:/usr/bin:/bin:{}/.local/bin", user.home),
            )
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "/usr/bin/false")
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("failed to run {program} as {}", user.name))?;
        if !output.status.success() {
            bail!(
                "{program} failed as {}: {}",
                user.name,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        return String::from_utf8(output.stdout)
            .with_context(|| format!("{program} output was not UTF-8"));
    }

    let mut stdout = tempfile::NamedTempFile::new_in(cwd)
        .with_context(|| format!("failed to create capture file in {cwd}"))?;
    let mut stderr = tempfile::NamedTempFile::new_in(cwd)
        .with_context(|| format!("failed to create capture file in {cwd}"))?;
    prepare_capture_file(stdout.as_file(), user)?;
    prepare_capture_file(stderr.as_file(), user)?;
    let stdout_path = Utf8PathBuf::from_path_buf(stdout.path().to_path_buf())
        .map_err(|path| anyhow::anyhow!("capture path is not UTF-8: {path:?}"))?;
    let stderr_path = Utf8PathBuf::from_path_buf(stderr.path().to_path_buf())
        .map_err(|path| anyhow::anyhow!("capture path is not UTF-8: {path:?}"))?;
    let result = run_as_user(UserCommand {
        user,
        program,
        args,
        cwd,
        stdout_file: Some(&stdout_path),
        stderr_file: Some(&stderr_path),
    });
    let captured_stdout = read_capture(&mut stdout)?;
    let captured_stderr = read_capture(&mut stderr)?;
    if let Err(error) = result {
        if captured_stderr.trim().is_empty() {
            return Err(error);
        }
        bail!("{error:#}: {}", captured_stderr.trim());
    }
    Ok(captured_stdout)
}

pub fn require_root() -> Result<()> {
    if !Uid::effective().is_root() {
        bail!("this command must be run as root");
    }
    Ok(())
}

fn prepare_capture_file(file: &fs::File, user: &BuildUser) -> Result<()> {
    // SAFETY: file is a descriptor for a newly created capture file.
    if unsafe { libc::fchown(file.as_raw_fd(), user.uid, user.gid) } < 0 {
        return Err(io::Error::last_os_error()).context("failed to chown capture file");
    }
    // SAFETY: file is a descriptor for a newly created capture file.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } < 0 {
        return Err(io::Error::last_os_error()).context("failed to chmod capture file");
    }
    Ok(())
}

fn read_capture(file: &mut tempfile::NamedTempFile) -> Result<String> {
    file.as_file_mut().seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    file.as_file_mut().read_to_string(&mut text)?;
    Ok(text)
}

fn child_exec_or_exit(
    exec: &mut PreparedExec,
    uid: libc::uid_t,
    gid: libc::gid_t,
    error_fd: RawFd,
) -> ! {
    let active_error_fd = match prepare_child_error_fd(error_fd) {
        Ok(fd) => fd,
        Err(errno) => {
            write_child_exec_error(error_fd, ChildExecError::new(1, errno));
            unsafe { libc::_exit(127) }
        }
    };
    let result = child_exec(exec, uid, gid);
    write_child_exec_error(active_error_fd, ChildExecError::new(result.0, result.1));
    unsafe { libc::_exit(127) }
}

fn child_exec(exec: &mut PreparedExec, uid: libc::uid_t, gid: libc::gid_t) -> (u8, i32) {
    if let Err(errno) = raw_close_extra_fds() {
        return (2, errno);
    }
    if let Err(errno) = raw_setgroups_empty() {
        return (3, errno);
    }
    if let Err(errno) = raw_setgid(gid) {
        return (4, errno);
    }
    if let Err(errno) = raw_setuid(uid) {
        return (5, errno);
    }
    if let Err(errno) = raw_chdir(exec.cwd.as_ptr()) {
        return (6, errno);
    }
    let stdin_fd = match raw_open_stdin_null() {
        Ok(fd) => fd,
        Err(errno) => return (7, errno),
    };
    if let Err(errno) = raw_dup2(stdin_fd, libc::STDIN_FILENO) {
        return (8, errno);
    }
    let _ = raw_close(stdin_fd);
    if let Some(stdout_file) = exec.stdout_file.as_ref() {
        let fd = match raw_open_stdout_file(stdout_file.as_ptr()) {
            Ok(fd) => fd,
            Err(errno) => return (9, errno),
        };
        if let Err(errno) = raw_dup2(fd, libc::STDOUT_FILENO) {
            return (10, errno);
        }
        let _ = raw_close(fd);
    }
    if let Some(stderr_file) = exec.stderr_file.as_ref() {
        let fd = match raw_open_stderr_file(stderr_file.as_ptr()) {
            Ok(fd) => fd,
            Err(errno) => return (11, errno),
        };
        if let Err(errno) = raw_dup2(fd, libc::STDERR_FILENO) {
            return (12, errno);
        }
        let _ = raw_close(fd);
    }
    raw_execve(exec)
}

impl ChildExecError {
    fn new(step: u8, errno: i32) -> Self {
        Self {
            step,
            _padding: [0; 3],
            errno,
        }
    }
}

fn write_child_exec_error(fd: RawFd, error: ChildExecError) {
    // SAFETY: this is the clone3 child side before exec/_exit. The buffer is a
    // plain C representation and write is a direct syscall.
    unsafe {
        let ptr = (&error as *const ChildExecError).cast::<libc::c_void>();
        let _ = libc::write(fd, ptr, std::mem::size_of::<ChildExecError>());
    }
}

fn copy_host_locale_env(env: &mut Vec<CString>) -> Result<()> {
    for (key, value) in std::env::vars() {
        if key == "LANG" || key == "LANGUAGE" || key.starts_with("LC_") {
            env.push(CString::new(format!("{key}={value}"))?);
        }
    }
    Ok(())
}

fn resolve_program(program: &str) -> PathBuf {
    if program.contains('/') {
        PathBuf::from(program)
    } else {
        PathBuf::from("/usr/bin").join(program)
    }
}

fn cstring_path(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow::anyhow!("path contains NUL: {path:?}"))
}

fn pipe_cloexec() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 writes two fds into the provided fixed-size array.
    let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if result < 0 {
        return Err(io::Error::last_os_error()).context("failed to create exec error pipe");
    }
    // SAFETY: pipe2 returned two owned fds on success.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((read, write))
}

fn prepare_child_error_fd(error_fd: RawFd) -> Result<RawFd, i32> {
    if error_fd != CHILD_ERROR_FD {
        raw_dup2(error_fd, CHILD_ERROR_FD)?;
    }
    raw_fcntl_setfd(CHILD_ERROR_FD, libc::FD_CLOEXEC)?;
    Ok(CHILD_ERROR_FD)
}

fn read_child_exec_status(
    error_read: &mut OwnedFd,
    child_pid: libc::pid_t,
    program: &str,
) -> Result<()> {
    let mut file = fs::File::from(error_read.try_clone()?);
    let mut bytes = [0_u8; std::mem::size_of::<ChildExecError>()];
    let read = file.read(&mut bytes)?;
    if read == 0 {
        return Ok(());
    }
    if read != bytes.len() {
        bail!("child {child_pid} reported truncated exec error for {program}");
    }
    let step = bytes[0];
    let errno = i32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    bail!(
        "child {child_pid} failed before exec of {program} at step {step}: {}",
        io::Error::from_raw_os_error(errno)
    );
}

fn wait_for_child(pid: libc::pid_t) -> Result<libc::c_int> {
    loop {
        let mut status = 0;
        // SAFETY: waitpid is called for the direct child pid returned by clone3.
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error).context("failed to wait for child");
        }
        return Ok(status);
    }
}

fn exit_code(status: libc::c_int) -> Option<i32> {
    if libc::WIFEXITED(status) {
        Some(libc::WEXITSTATUS(status))
    } else {
        None
    }
}

fn term_signal(status: libc::c_int) -> Option<i32> {
    if libc::WIFSIGNALED(status) {
        Some(libc::WTERMSIG(status))
    } else {
        None
    }
}

fn raw_dup2(source: RawFd, target: RawFd) -> Result<(), i32> {
    if source == target {
        return Ok(());
    }
    let result = unsafe { libc::dup2(source, target) };
    errno_result(result).map(|_| ())
}

fn raw_fcntl_setfd(fd: RawFd, flags: libc::c_int) -> Result<(), i32> {
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags) };
    errno_result(result).map(|_| ())
}

fn raw_close_extra_fds() -> Result<(), i32> {
    let result = unsafe { libc::syscall(libc::SYS_close_range, 4_u32, u32::MAX, 0_u32) };
    if result < 0 {
        Err(last_errno())
    } else {
        Ok(())
    }
}

fn raw_setgroups_empty() -> Result<(), i32> {
    let result = unsafe { libc::setgroups(0, std::ptr::null()) };
    errno_result(result).map(|_| ())
}

fn raw_setgid(gid: libc::gid_t) -> Result<(), i32> {
    let result = unsafe { libc::setgid(gid) };
    errno_result(result).map(|_| ())
}

fn raw_setuid(uid: libc::uid_t) -> Result<(), i32> {
    let result = unsafe { libc::setuid(uid) };
    errno_result(result).map(|_| ())
}

fn raw_chdir(path: *const libc::c_char) -> Result<(), i32> {
    let result = unsafe { libc::chdir(path) };
    errno_result(result).map(|_| ())
}

fn raw_open_stdin_null() -> Result<RawFd, i32> {
    let path = c"/dev/null";
    let result = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    errno_result(result)
}

fn raw_open_stdout_file(path: *const libc::c_char) -> Result<RawFd, i32> {
    let result = unsafe {
        libc::open(
            path,
            libc::O_CREAT | libc::O_TRUNC | libc::O_WRONLY | libc::O_CLOEXEC,
            0o644,
        )
    };
    errno_result(result)
}

fn raw_open_stderr_file(path: *const libc::c_char) -> Result<RawFd, i32> {
    let result = unsafe {
        libc::open(
            path,
            libc::O_CREAT | libc::O_APPEND | libc::O_WRONLY | libc::O_CLOEXEC,
            0o644,
        )
    };
    errno_result(result)
}

fn raw_close(fd: RawFd) -> Result<(), i32> {
    let result = unsafe { libc::close(fd) };
    errno_result(result).map(|_| ())
}

fn raw_execve(exec: &PreparedExec) -> (u8, i32) {
    unsafe {
        libc::execve(
            exec.path.as_ptr(),
            exec.argv_ptrs.as_ptr(),
            exec.env_ptrs.as_ptr(),
        );
    }
    (13, last_errno())
}

fn errno_result(result: libc::c_int) -> Result<libc::c_int, i32> {
    if result < 0 {
        Err(last_errno())
    } else {
        Ok(result)
    }
}

fn last_errno() -> i32 {
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}
