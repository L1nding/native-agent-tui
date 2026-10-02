use std::ffi::{OsStr, OsString};
use std::io;
use std::marker::PhantomData;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{
    AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, IntoRawHandle, OwnedHandle,
};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::ptr;

use tokio::fs::File;
use tokio::task::JoinHandle;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Globalization::{CompareStringOrdinal, CSTR_EQUAL};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::*;

use super::{Command, Input};

pub(crate) struct Child {
    process: OwnedHandle,
    job: Option<OwnedHandle>,
    waiter: Option<JoinHandle<io::Result<ExitStatus>>>,
    status: Option<ExitStatus>,
    pub(crate) stdin: Option<File>,
    pub(crate) stdout: Option<File>,
    pub(crate) stderr: Option<File>,
}

pub(crate) struct Suspended {
    child: Child,
    thread: OwnedHandle,
}

impl Suspended {
    pub(crate) fn resume(self) -> io::Result<Child> {
        // SAFETY: this uniquely owned primary thread was created suspended.
        if unsafe { ResumeThread(self.thread.as_raw_handle()) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(self.child)
    }

    // Used by the standalone native fixture to inspect the creation boundary.
    #[allow(dead_code)]
    pub(crate) fn process(&self) -> &OwnedHandle {
        &self.child.process
    }
}

impl Child {
    pub(crate) fn close_tree(&mut self) {
        self.job.take();
    }

    pub(crate) fn start_kill(&mut self) -> io::Result<()> {
        self.close_tree();
        // This owner holds the only, non-inheritable job lease. Closing it requests
        // termination of the entire tree. wait() confirms exit; a second kill can
        // otherwise report AccessDenied while the first termination is in flight.
        Ok(())
    }

    pub(crate) async fn kill(&mut self) -> io::Result<()> {
        self.shutdown_tree().await?;
        self.wait().await.map(|_| ())
    }

    pub(crate) async fn shutdown_tree(&mut self) -> io::Result<()> {
        let Some(job) = &self.job else {
            return Ok(());
        };
        // SAFETY: this owner retains the only job lease through termination and query.
        if unsafe { TerminateJobObject(job.as_raw_handle(), 1) } == 0 {
            let error = io::Error::last_os_error();
            self.close_tree();
            return Err(error);
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION =
                    unsafe { std::mem::zeroed() };
                // SAFETY: initialized output structure, exact size and a retained job.
                if unsafe {
                    QueryInformationJobObject(
                        job.as_raw_handle(),
                        JobObjectBasicAccountingInformation,
                        (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                        std::mem::size_of_val(&accounting) as u32,
                        ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if accounting.ActiveProcesses == 0 {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "owned process tree exit was not confirmed",
            ))
        });
        self.close_tree();
        result
    }

    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        if self.waiter.is_none() {
            let process = self.process.try_clone()?;
            self.waiter = Some(tokio::task::spawn_blocking(move || {
                // SAFETY: a retained process handle remains valid until the wait finishes.
                if unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) }
                    != WAIT_OBJECT_0
                {
                    return Err(io::Error::last_os_error());
                }
                let mut code = 0;
                if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(ExitStatus::from_raw(code))
            }));
        }
        let result = self.waiter.as_mut().unwrap().await;
        self.waiter.take();
        let status = result.map_err(io::Error::other)??;
        self.status = Some(status);
        Ok(status)
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.start_kill();
    }
}

fn own(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: callers transfer a newly created, uniquely owned handle here.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

fn inheritable(handle: OwnedHandle) -> io::Result<OwnedHandle> {
    // SAFETY: the handle is owned; only its inheritance flag is changed.
    if unsafe {
        SetHandleInformation(
            handle.as_raw_handle(),
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(handle)
}

fn job() -> io::Result<OwnedHandle> {
    // SAFETY: null security/name makes this an unnamed non-inheritable owned job.
    let job = own(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: initialized limits and exact structure size; live owned job handle.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

struct Attributes<'a> {
    storage: Vec<usize>,
    stdio: Box<[HANDLE]>,
    jobs: Box<[HANDLE]>,
    lease: PhantomData<&'a OwnedHandle>,
}

impl<'a> Attributes<'a> {
    fn new(stdio: [BorrowedHandle<'a>; 3], job: BorrowedHandle<'a>) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: sizing call intentionally has no buffer; only the size is read.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0; bytes.div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: allocation is pointer-aligned, large enough, and remains fixed.
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 2, 0, &mut bytes)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut attributes = Self {
            storage,
            stdio: stdio.map(|handle| handle.as_raw_handle()).into(),
            jobs: vec![job.as_raw_handle()].into_boxed_slice(),
            lease: PhantomData,
        };
        let list = attributes.storage.as_mut_ptr().cast();
        for (attribute, values) in [
            (PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &*attributes.stdio),
            (PROC_THREAD_ATTRIBUTE_JOB_LIST, &*attributes.jobs),
        ] {
            // SAFETY: heap allocations keep value addresses stable even if Self moves;
            // the borrowed handle leases outlive this list and CreateProcessW.
            if unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    attribute as usize,
                    values.as_ptr().cast_mut().cast(),
                    std::mem::size_of_val(values),
                    ptr::null_mut(),
                    ptr::null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(attributes)
    }
}

impl Drop for Attributes<'_> {
    fn drop(&mut self) {
        // SAFETY: list was initialized and its allocation is still valid.
        unsafe { DeleteProcThreadAttributeList(self.storage.as_mut_ptr().cast()) };
    }
}

pub(crate) fn suspend(command: Command, input: Input) -> io::Result<Suspended> {
    let job = job()?;
    let command = command.0;
    let mut environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        if key.is_empty() || key.encode_wide().any(|c| c == 0 || c == b'=' as u16) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid process environment name",
            ));
        }
        environment.retain(|(name, _)| compare(name, key) != std::cmp::Ordering::Equal);
        if let Some(value) = value {
            environment.push((key.to_owned(), value.to_owned()));
        }
    }
    environment.sort_by(|(a, _), (b, _)| compare(a, b));
    let path = environment
        .iter()
        .find(|(key, _)| compare(key, OsStr::new("PATH")) == std::cmp::Ordering::Equal)
        .map(|(_, value)| value);
    let executable = resolve(command.get_program(), path)?;
    let (application, mut line) = command_line(&executable, command.get_args())?;
    let application = wide(application.as_os_str())?;
    let directory = command
        .get_current_dir()
        .map(|path| wide(path.as_os_str()))
        .transpose()?;
    let mut block = Vec::new();
    for (key, value) in &environment {
        block.extend(wide(key)?.into_iter().take_while(|c| *c != 0));
        block.push(b'=' as u16);
        block.extend(wide(value)?);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    let (stdin, child_in) = match input {
        Input::Pipe => {
            let (read, write) = io::pipe()?;
            // SAFETY: each pipe endpoint's handle is transferred exactly once.
            let child =
                inheritable(unsafe { OwnedHandle::from_raw_handle(read.into_raw_handle()) })?;
            let parent = pipe_file(write.into_raw_handle());
            (Some(parent), child)
        }
        Input::Null => {
            let security = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: ptr::null_mut(),
                bInheritHandle: 1,
            };
            let nul = wide(OsStr::new("NUL"))?;
            // SAFETY: valid terminated name/security; resulting child-only handle is owned.
            let child = own(unsafe {
                CreateFileW(
                    nul.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    &security,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    ptr::null_mut(),
                )
            })?;
            (None, child)
        }
    };
    let (stdout_read, stdout_write) = io::pipe()?;
    let (stderr_read, stderr_write) = io::pipe()?;
    // SAFETY: newly created endpoints are transferred to the matching owner once.
    let child_out =
        inheritable(unsafe { OwnedHandle::from_raw_handle(stdout_write.into_raw_handle()) })?;
    let child_err =
        inheritable(unsafe { OwnedHandle::from_raw_handle(stderr_write.into_raw_handle()) })?;
    let stdout = pipe_file(stdout_read.into_raw_handle());
    let stderr = pipe_file(stderr_read.into_raw_handle());
    let handles = [
        child_in.as_raw_handle(),
        child_out.as_raw_handle(),
        child_err.as_raw_handle(),
    ];
    let mut attributes = Attributes::new(
        [
            child_in.as_handle(),
            child_out.as_handle(),
            child_err.as_handle(),
        ],
        job.as_handle(),
    )?;
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.storage.as_mut_ptr().cast();
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: UTF-16 buffers, live attribute arrays and child-only stdio handles
    // remain valid through the call. Job assignment is part of process creation;
    // there is no running or suspended unowned child after a successful call.
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT
                | CREATE_SUSPENDED,
            block.as_ptr().cast(),
            directory.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
            &startup.StartupInfo,
            &mut process,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    drop(attributes);
    let process_handle = own(process.hProcess)?;
    let thread = own(process.hThread)?;
    Ok(Suspended {
        child: Child {
            process: process_handle,
            job: Some(job),
            waiter: None,
            status: None,
            stdin,
            stdout: Some(stdout),
            stderr: Some(stderr),
        },
        thread,
    })
}

fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = text.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process argument contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}

fn pipe_file(handle: std::os::windows::io::RawHandle) -> File {
    // SAFETY: caller transfers a unique parent pipe endpoint to this owner.
    let mut file = File::from_std(unsafe { std::fs::File::from_raw_handle(handle) });
    file.set_max_buf_size(8192);
    file
}

fn compare(a: &OsStr, b: &OsStr) -> std::cmp::Ordering {
    let a: Vec<u16> = a.encode_wide().chain(std::iter::once(0)).collect();
    let b: Vec<u16> = b.encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: both UTF-16 buffers are terminated; comparison does not retain pointers.
    let result = unsafe { CompareStringOrdinal(a.as_ptr(), -1, b.as_ptr(), -1, 1) };
    result.cmp(&CSTR_EQUAL)
}

fn resolve(program: &OsStr, path: Option<&OsString>) -> io::Result<PathBuf> {
    let program = Path::new(program);
    let program = if program.extension().is_none() {
        program.with_extension("exe")
    } else {
        program.to_owned()
    };
    if program.components().count() > 1 || program.is_absolute() {
        return program.canonicalize();
    }
    let mut directories = vec![std::env::current_dir()?];
    if let Some(parent) = std::env::current_exe()?.parent() {
        directories.push(parent.to_owned());
    }
    directories.push(system_directory()?);
    if let Some(path) = path {
        directories.extend(std::env::split_paths(path));
    }
    for directory in directories {
        let candidate = directory.join(&program);
        if candidate.is_file() {
            return candidate.canonicalize();
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "process executable was not found",
    ))
}

fn system_directory() -> io::Result<PathBuf> {
    let mut value = vec![0u16; 32768];
    // SAFETY: initialized UTF-16 allocation and its exact capacity.
    let length = unsafe { GetSystemDirectoryW(value.as_mut_ptr(), value.len() as u32) } as usize;
    if length == 0 || length >= value.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(PathBuf::from(OsString::from_wide(&value[..length])))
}

fn command_line<'a>(
    executable: &Path,
    arguments: impl Iterator<Item = &'a OsStr>,
) -> io::Result<(PathBuf, Vec<u16>)> {
    let batch = executable.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    });
    let application = if batch {
        system_directory()?.join("cmd.exe")
    } else {
        executable.to_owned()
    };
    let mut line = Vec::new();
    quote(application.as_os_str(), &mut line, false)?;
    if batch {
        line.extend(" /e:on /v:off /d /c \"".encode_utf16());
        let path = executable.as_os_str().encode_wide().collect::<Vec<_>>();
        let path = if path.starts_with(&"\\\\?\\UNC\\".encode_utf16().collect::<Vec<_>>()) {
            let mut normal = "\\\\".encode_utf16().collect::<Vec<_>>();
            normal.extend_from_slice(&path[8..]);
            normal
        } else if path.starts_with(&"\\\\?\\".encode_utf16().collect::<Vec<_>>()) {
            path[4..].to_vec()
        } else {
            path
        };
        quote(&OsString::from_wide(&path), &mut line, true)?;
    }
    for argument in arguments {
        line.push(b' ' as u16);
        quote(argument, &mut line, batch)?;
    }
    if batch {
        line.push(b'"' as u16);
    }
    let limit = if batch { 8191 } else { 32767 };
    if line.len() >= limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process command line exceeds executable or batch limit",
        ));
    }
    line.push(0);
    Ok((application, line))
}

fn quote(argument: &OsStr, output: &mut Vec<u16>, batch: bool) -> io::Result<()> {
    let value = wide(argument)?;
    output.push(b'"' as u16);
    let mut slashes = 0;
    for &unit in &value[..value.len() - 1] {
        if batch && matches!(unit, 10 | 13) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "batch arguments cannot contain newlines",
            ));
        }
        if unit == b'\\' as u16 {
            slashes += 1;
        } else {
            if unit == b'"' as u16 {
                output.extend(std::iter::repeat_n(b'\\' as u16, slashes));
                output.push(if batch { b'"' as u16 } else { b'\\' as u16 });
            } else if batch && unit == b'%' as u16 {
                // Empty CD substring prevents cmd expanding an environment reference.
                output.extend("%%cd:~,".encode_utf16());
            }
            slashes = 0;
        }
        output.push(unit);
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes));
    output.push(b'"' as u16);
    Ok(())
}
