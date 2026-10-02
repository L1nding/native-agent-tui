//! Native Windows process ownership fixture; no Codex or model calls.
#[cfg(all(windows, not(test)))]
#[path = "../src/owned_process.rs"]
mod owned_process;

#[cfg(all(windows, not(test)))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Read, Write};
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessHandleCount, GetProcessId, WaitForSingleObject,
    };

    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let mode = arguments.first().ok_or("missing fixture mode")?;
    let root = std::path::PathBuf::from(
        std::env::var_os("NATIVE_PROCESS_FIXTURE_ROOT").ok_or("missing fixture root")?,
    );
    let membership = |process| {
        let mut inside = 0;
        // SAFETY: callers supply a retained live process handle or current-process handle.
        if unsafe { IsProcessInJob(process, std::ptr::null_mut(), &mut inside) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(inside != 0)
        }
    };
    if mode == "child"
        || mode == "grandchild"
        || mode == "arguments"
        || mode == "catalog"
        || mode == "exit-259"
    {
        let in_job = membership(unsafe { GetCurrentProcess() })?;
        let mut result = serde_json::json!({"pid":std::process::id(),"in_job":in_job,"arguments":&arguments[1..],"cwd":std::env::current_dir()?});
        if mode == "catalog" {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input)?;
            result["stdin_bytes"] = input.len().into();
        }
        if mode == "child" {
            let descendant = std::process::Command::new(std::env::current_exe()?)
                .arg("grandchild")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x08000000)
                .spawn()?;
            result["descendant_pid"] = descendant.id().into();
            drop(descendant); // The inherited outer job remains its lifetime owner.
        }
        std::fs::write(
            root.join(format!("{mode}.json")),
            serde_json::to_vec(&result)?,
        )?;
        println!("{result}");
        std::io::stdout().flush()?;
        if mode == "exit-259" {
            std::process::exit(259);
        }
        if mode == "arguments" || mode == "catalog" {
            return Ok(());
        }
        loop {
            std::thread::park();
        }
    }
    if mode == "invalid-input" {
        let executable = std::env::current_exe()?;
        let batch = root.join("wrapper.cmd");
        std::fs::write(&batch, b"@echo off\nexit /b 99\n")?;
        for (program, value) in [
            (&executable, "embedded\0nul".to_owned()),
            (&executable, "x".repeat(32768)),
            (&batch, "embedded\nnewline".to_owned()),
            (&batch, "embedded\rcarriage".to_owned()),
            (&batch, "x".repeat(8192)),
        ] {
            let mut command = owned_process::Command::new(program);
            command.args([value]);
            match owned_process::spawn(command, owned_process::Input::Pipe) {
                Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
                _ => return Err("invalid command was not rejected before creation".into()),
            }
        }
        println!("{}", serde_json::json!({"rejected":5}));
        return Ok(());
    }
    if mode == "creation-failures" {
        let invalid = root.join("invalid.exe");
        std::fs::write(&invalid, b"invalid executable image")?;
        let count = || -> Result<u32, std::io::Error> {
            let mut count = 0;
            if unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) } == 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(count)
            }
        };
        let cold = count()?;
        if owned_process::spawn(
            owned_process::Command::new(&invalid),
            owned_process::Input::Pipe,
        )
        .is_ok()
        {
            return Err("invalid image unexpectedly launched during warmup".into());
        }
        let before = count()?;
        for _ in 0..100 {
            if owned_process::spawn(
                owned_process::Command::new(&invalid),
                owned_process::Input::Pipe,
            )
            .is_ok()
            {
                return Err("invalid image unexpectedly launched".into());
            }
        }
        println!(
            "{}",
            serde_json::json!({"cold":cold,"before":before,"after":count()?,"failures":100})
        );
        return Ok(());
    }
    let program = if mode == "batch" {
        root.join("wrapper.cmd")
    } else {
        std::env::current_exe()?
    };
    let mut command = owned_process::Command::new(program);
    command.current_dir(&root);
    let child_mode = if mode == "batch" || mode == "executable" {
        "arguments"
    } else if mode == "exit-code" {
        "exit-259"
    } else if mode == "null-input" {
        "catalog"
    } else {
        "child"
    };
    command.args([child_mode]);
    command.args(&arguments[1..]);
    let input = if mode == "null-input" {
        owned_process::Input::Null
    } else {
        owned_process::Input::Pipe
    };
    if mode == "suspended" || mode == "reject" {
        let pending = owned_process::windows::suspend(command, input)?;
        let process = pending.process().try_clone()?;
        let pid = unsafe { GetProcessId(process.as_raw_handle()) };
        println!(
            "{}",
            serde_json::json!({"pid":pid,"in_job":membership(process.as_raw_handle())?,"stage":"suspended"})
        );
        std::io::stdout().flush()?;
        let mut control = String::new();
        std::io::stdin().read_line(&mut control)?;
        drop(pending);
        if unsafe { WaitForSingleObject(process.as_raw_handle(), 3000) } != WAIT_OBJECT_0 {
            return Err("suspended child survived owner rejection".into());
        }
        return Ok(());
    }
    let mut child = owned_process::spawn(command, input)?;
    child.stdin.take();
    let mut errors = child.stderr.take().ok_or("missing stderr")?;
    let drain = tokio::spawn(async move {
        let mut buffer = [0; 8192];
        while matches!(errors.read(&mut buffer).await, Ok(n) if n > 0) {}
    });
    let mut output = BufReader::new(child.stdout.take().ok_or("missing stdout")?);
    let mut report = String::new();
    output.read_line(&mut report).await?;
    if report.is_empty() {
        return Err("child did not report its startup state".into());
    }
    print!("{report}");
    std::io::stdout().flush()?;
    if mode == "exit-code" {
        if child.wait().await?.code() != Some(259) {
            return Err("exit 259 was mistaken for a running process".into());
        }
        child.kill().await?;
    } else if mode == "batch" || mode == "executable" || mode == "null-input" {
        let status = child.wait().await?;
        if !status.success() || child.wait().await? != status {
            return Err("fixture child failed".into());
        }
    } else {
        if mode == "wait-cancelled"
            && tokio::time::timeout(std::time::Duration::from_millis(50), child.wait())
                .await
                .is_ok()
        {
            return Err("fixture child ended before the cancelled wait".into());
        }
        let mut control = String::new();
        std::io::stdin().read_line(&mut control)?;
        child.kill().await?;
    }
    child.close_tree();
    drain.abort();
    let _ = drain.await;
    Ok(())
}

#[cfg(any(not(windows), test))]
fn main() {}
