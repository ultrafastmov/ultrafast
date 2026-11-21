//! `uf`: the `ultrafast` command, short. Runs the `ultrafast` binary that is
//! installed next to it with the same arguments.

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent()
                .map(|d| d.join(format!("ultrafast{}", std::env::consts::EXE_SUFFIX)))
        })
        .filter(|p| p.exists())
        .unwrap_or_else(|| "ultrafast".into());
    let mut cmd = Command::new(&exe);
    cmd.args(std::env::args_os().skip(1));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        eprintln!("uf: cannot run {}: {err}", exe.display());
        ExitCode::from(127)
    }
    #[cfg(not(unix))]
    match cmd.status() {
        Ok(s) => ExitCode::from(s.code().unwrap_or(1) as u8),
        Err(err) => {
            eprintln!("uf: cannot run {}: {err}", exe.display());
            ExitCode::from(127)
        }
    }
}
