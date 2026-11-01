//! Lets Run as administrator start without a UAC prompt, through a scheduled task that Windows runs elevated.
//! Registering it needs admin, so the first elevated start does that, and starting it afterwards doesn't.
//! Only an install in Program Files gets a task. Anything can rewrite a copy in a user folder, and a
//! task pointing there would hand admin rights to whatever replaced it.
use crate::win32::init_com_for_thread;
use std::path::Path;
use std::thread;
use windows::core::{BSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::TaskScheduler::{ITaskFolder, ITaskService, TaskScheduler, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};

fn xml_escaped(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// No triggers: the normal startup entry and shortcuts start this on demand.
fn task_xml(sid: &str, exe: &str, dir: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts Afterclip as administrator without a UAC prompt. Managed by the app's Run as administrator setting.</Description></RegistrationInfo>
  <Triggers />
  <Principals><Principal id="Owner"><UserId>{sid}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Owner"><Exec><Command>{}</Command><WorkingDirectory>{}</WorkingDirectory></Exec></Actions>
</Task>"#,
        xml_escaped(exe),
        xml_escaped(dir),
    )
}

fn user_sid() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        // u64s keep the buffer aligned for the structure written into it.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let read = GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr().cast()), needed, &mut needed);
        let _ = CloseHandle(token);
        read.ok()?;
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

fn task_name() -> Option<BSTR> {
    Some(BSTR::from(format!("Afterclip-Elevated-{}", user_sid()?)))
}

fn is_inside(path: &str, folder: &str) -> bool {
    path.to_lowercase().starts_with(&format!("{}\\", folder.trim_end_matches('\\').to_lowercase()))
}

/// Asks Windows rather than an environment variable, which anything could set.
fn is_protected_install(exe: &Path) -> bool {
    unsafe {
        let Ok(folder) = SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KNOWN_FOLDER_FLAG(0), None) else { return false };
        let program_files = folder.to_string().unwrap_or_default();
        CoTaskMemFree(Some(folder.0.cast()));
        !program_files.is_empty() && is_inside(&exe.to_string_lossy(), &program_files)
    }
}

fn root_folder() -> windows::core::Result<ITaskFolder> {
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let none = VARIANT::default();
        service.Connect(&none, &none, &none, &none)?;
        service.GetFolder(&BSTR::from("\\"))
    }
}

/// The caller's thread may already be set up for COM in a way the scheduler doesn't want.
fn on_com_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    thread::spawn(move || {
        init_com_for_thread();
        work()
    })
    .join()
    .ok()
}

/// Starts the elevated copy. False if there's no task to start, so the caller can fall back to UAC.
pub fn start() -> bool {
    on_com_thread(|| -> windows::core::Result<()> {
        let name = task_name().ok_or_else(windows::core::Error::empty)?;
        unsafe {
            let task = root_folder()?.GetTask(&name)?;
            task.Run(&VARIANT::default())?;
        }
        Ok(())
    })
    .is_some_and(|result| result.is_ok())
}

/// Makes the task match the setting. Call only while elevated.
pub fn sync(wanted: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if wanted && !is_protected_install(&exe) {
        return Ok(());
    }
    on_com_thread(move || -> windows::core::Result<()> {
        let name = task_name().ok_or_else(windows::core::Error::empty)?;
        unsafe {
            let folder = root_folder()?;
            if !wanted {
                // Already being absent is the goal, not a failure.
                let _ = folder.DeleteTask(&name, 0);
                return Ok(());
            }
            let sid = user_sid().ok_or_else(windows::core::Error::empty)?;
            let dir = exe.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let xml = BSTR::from(task_xml(&sid, &exe.to_string_lossy(), &dir));
            let none = VARIANT::default();
            folder.RegisterTask(&name, &xml, TASK_CREATE_OR_UPDATE.0, &none, &none, TASK_LOGON_INTERACTIVE_TOKEN, &none)?;
        }
        Ok(())
    })
    .ok_or("The task thread stopped")?
    .map_err(|e| e.to_string())
}

/// Elevated, the app shouldn't trust settings that anything running as the user could have set.
/// These tell WebView2 which browser to run and how, which would otherwise run with admin rights.
pub fn clear_unsafe_environment() {
    let names: Vec<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            let name = name.to_string_lossy().to_uppercase();
            name.starts_with("WEBVIEW2_") || name.starts_with("COREWEBVIEW2_")
        })
        .collect();
    for name in names {
        std::env::remove_var(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_runs_elevated_for_one_user_at_normal_priority() {
        let xml = task_xml("S-1-5-21-1", r"C:\Program Files\Afterclip\afterclip.exe", r"C:\Program Files\Afterclip");
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<UserId>S-1-5-21-1</UserId>"));
        assert!(xml.contains(r"<Command>C:\Program Files\Afterclip\afterclip.exe</Command>"));
        assert!(xml.contains("<Priority>4</Priority>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    }

    #[test]
    fn task_paths_are_escaped() {
        assert!(task_xml("s", r"C:\A&B\a.exe", r"C:\A&B").contains(r"<Command>C:\A&amp;B\a.exe</Command>"));
    }

    #[test]
    fn only_program_files_counts_as_protected() {
        let folder = r"C:\Program Files";
        assert!(is_inside(r"c:\program files\Afterclip\afterclip.exe", folder));
        assert!(!is_inside(r"C:\Program Files Evil\afterclip.exe", folder));
        assert!(!is_inside(r"C:\Users\Josh\AppData\Local\Afterclip\afterclip.exe", folder));
    }

    #[test]
    fn finds_the_current_users_sid() {
        assert!(user_sid().is_some_and(|sid| sid.starts_with("S-1-")));
    }
}
