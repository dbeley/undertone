//! Child-process helpers.

use std::os::unix::process::CommandExt;
use std::process::Command;

/// Detach a child from the controlling terminal.
///
/// A child that still owns `/dev/tty` can probe it — chafa asks for the
/// terminal's foreground/background colours and its cell size — and the
/// terminal's replies are delivered to *our* stdin, where they are decoded as
/// keystrokes. An `OSC 10 ; rgb:…` report becomes `Alt+]`, then `1`, `0`, `;`,
/// `r`, `g`, `b`, … — and the `r` cycles repeat.
///
/// With no controlling terminal there is nothing to probe, so the replies
/// never exist. Failure is ignored: a detached-but-working child is better
/// than no child at all.
pub fn detach_from_terminal(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detached_child_has_no_controlling_terminal() {
        // `tty` exits non-zero with "not a tty" when there is no controlling
        // terminal, which is exactly the state that stops chafa probing.
        let mut sh = Command::new("sh");
        sh.args(["-c", "tty 2>&1 || echo none"]);
        detach_from_terminal(&mut sh);
        let output = sh.output().expect("spawn sh");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("not a tty") || stdout.contains("none"),
            "detached child should have no tty, got {stdout:?}"
        );
    }

    #[test]
    fn the_terminal_attached_child_still_has_one() {
        // The guard above is only meaningful if an undetached child differs.
        // Skip when the test suite is not itself running on a terminal.
        let output = Command::new("sh")
            .args(["-c", "tty 2>&1 || echo none"])
            .output()
            .expect("spawn sh");
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("not a tty") || stdout.contains("none") {
            return;
        }
        assert!(stdout.starts_with("/dev/"), "got {stdout:?}");
    }
}
