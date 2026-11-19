use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::AppError;
use winsafe::{HPROCESS, co, prelude::*};

pub fn pid_alive(pid: u32) -> bool {
    match HPROCESS::OpenProcess(co::PROCESS::SYNCHRONIZE, false, pid) {
        Ok(handle) => handle.WaitForSingleObject(Some(0)) == Ok(co::WAIT::TIMEOUT),
        Err(error) => error == co::ERROR::ACCESS_DENIED,
    }
}

// Windows PowerShell ships with the OS. Pass the literal path separately so
// spaces, quotes and wildcard characters in a profile name stay data.
fn acl_command(path: &Path, script: &str) -> Result<bool, AppError> {
    let system =
        std::env::var_os("SystemRoot").ok_or_else(|| AppError::config("SystemRoot is missing"))?;
    let output =
        Command::new(PathBuf::from(system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("BOATPARTY_ACL_PATH", path)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| {
                AppError::io(
                    "cannot use Windows PowerShell to inspect file permissions",
                    error,
                )
            })?;
    if !output.status.success() && !output.stderr.is_empty() {
        return Err(AppError::config(format!(
            "cannot inspect or set the ACL on {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.status.success())
}

/// Give the current user full control and let new files inherit that rule.
/// Secure the directory before opening any file that will contain a secret.
pub(crate) fn secure_directory(path: &Path) -> Result<(), AppError> {
    std::fs::create_dir_all(path)
        .map_err(|error| AppError::io(format!("cannot create {}", path.display()), error))?;
    let ok = acl_command(
        path,
        r#"
        $ErrorActionPreference = 'Stop'
        $item = Get-Item -Force -LiteralPath $env:BOATPARTY_ACL_PATH
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { exit 1 }
        $user = [Security.Principal.WindowsIdentity]::GetCurrent().User
        $acl = [Security.AccessControl.DirectorySecurity]::new()
        $acl.SetAccessRuleProtection($true, $false)
        $rule = [Security.AccessControl.FileSystemAccessRule]::new($user, 'FullControl', 'ContainerInherit, ObjectInherit', 'None', 'Allow')
        $acl.AddAccessRule($rule)
        [IO.Directory]::SetAccessControl($item.FullName, $acl)
    "#,
    )?;
    if ok {
        Ok(())
    } else {
        Err(AppError::config(format!(
            "cannot secure the directory ACL on {}",
            path.display()
        )))
    }
}

pub(crate) fn private_acl(path: &Path) -> Result<bool, AppError> {
    acl_command(
        path,
        r#"
        $ErrorActionPreference = 'Stop'
        $sid = [Security.Principal.SecurityIdentifier]
        $user = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        $trusted = @($user, 'S-1-5-18', 'S-1-5-32-544')
        $acl = Get-Acl -LiteralPath $env:BOATPARTY_ACL_PATH
        if ($acl.GetOwner($sid).Value -notin $trusted) { exit 1 }
        foreach ($rule in $acl.GetAccessRules($true, $true, $sid)) {
            if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin $trusted) { exit 1 }
        }
    "#,
    )
}
