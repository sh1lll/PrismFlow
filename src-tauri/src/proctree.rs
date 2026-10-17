//! Stopping a program together with everything it started (an npm shim's
//! real binary, an agent's workers), so a skipped or timed-out test leaves
//! nothing running.

use tokio::process::Command;

/// Gives the program a process group of its own (Unix) so the whole tree
/// can be stopped. Call before `spawn`.
pub fn own_group(cmd: &mut Command) {
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(not(unix))]
    let _ = cmd;
}

/// Stops the tree when dropped, unless `finished` was called.
pub struct TreeGuard(Option<u32>);

impl TreeGuard {
    pub fn new(pid: Option<u32>) -> Self {
        Self(pid)
    }

    /// The program ended by itself; leave whatever it left running alone.
    pub fn finished(mut self) {
        self.0 = None;
    }
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        let Some(pid) = self.0 else { return };
        #[cfg(unix)]
        let _ = crate::spawn::command("kill").args(["-KILL", "--", &format!("-{pid}")]).status();
        #[cfg(windows)]
        let _ = crate::spawn::command("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).status();
    }
}
