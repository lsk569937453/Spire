use crate::vojo::cli::ReloadArgs;

/// Sends `SIGHUP` to a running Spire process.
///
/// The signal is what makes the process re-read its configuration file — the
/// same thing `systemd`'s `ExecReload=/bin/kill -HUP $MAINPID` and
/// `docker kill -s HUP` deliver. Going through `kill` keeps this free of
/// `unsafe` FFI and of a new dependency for a single syscall.
#[cfg(unix)]
pub fn handle_reload_command(args: ReloadArgs) -> Result<(), String> {
    let arguments = signal_arguments(args.pid);
    let status = std::process::Command::new("kill")
        .args(&arguments)
        .status()
        .map_err(|e| {
            format!(
                "Failed to run 'kill': {e}. Send the signal manually with 'kill -HUP {}'.",
                args.pid
            )
        })?;
    if status.success() {
        println!("Sent SIGHUP to process {}.", args.pid);
        Ok(())
    } else {
        Err(format!(
            "'kill -HUP {}' failed ({status}); is that PID a running Spire process?",
            args.pid
        ))
    }
}

/// There is no `SIGHUP` on Windows: a configuration change means a restart.
#[cfg(not(unix))]
pub fn handle_reload_command(args: ReloadArgs) -> Result<(), String> {
    let _ = args;
    Err(
        "'reload' is not supported on Windows (there is no SIGHUP); restart the process instead."
            .to_string(),
    )
}

#[cfg(unix)]
fn signal_arguments(pid: u32) -> Vec<String> {
    vec!["-HUP".to_string(), pid.to_string()]
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn reload_targets_the_requested_pid() {
        assert_eq!(signal_arguments(1234), vec!["-HUP", "1234"]);
    }
}
