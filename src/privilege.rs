use std::io;
use std::process::ExitStatus;

use crate::exec::{CommandSpec, Runner};

pub const NO_TTY_HINT: &str =
    "no terminal is available for the sudo prompt; run `sudo -v` first, then retry";

/// Run a CommandSpec with `sudo`.
///
/// The password prompt belongs to `sudo` so we don't need to capture
/// anything ourselves.
pub fn escalate(spec: CommandSpec) -> CommandSpec {
    spec.prefixed_with("sudo")
}

/// Whether stdin can be used for an interactive `sudo` prompt.
pub fn stdin_is_tty() -> bool {
    nix::unistd::isatty(io::stdin()).unwrap_or(false)
}

#[derive(Debug)]
pub enum PrivilegeError {
    NotATerminal { command: String },
    Io(io::Error),
}

impl std::fmt::Display for PrivilegeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivilegeError::NotATerminal { command } => {
                write!(formatter, "{NO_TTY_HINT}: {command}")
            }
            PrivilegeError::Io(error) => write!(formatter, "cannot run sudo: {error}"),
        }
    }
}

/// Refuse a privileged command when stdin cannot show a password prompt.
///
/// The refusal carries the exact command so the user can run it manually.
pub fn ensure_terminal(spec: &CommandSpec) -> Result<(), PrivilegeError> {
    if stdin_is_tty() {
        Ok(())
    } else {
        Err(PrivilegeError::NotATerminal {
            command: spec.display(),
        })
    }
}

/// Run a privileged command with a terminal attached.
pub fn run(spec: &CommandSpec) -> Result<ExitStatus, PrivilegeError> {
    ensure_terminal(spec)?;

    Runner::new(false)
        .run_interactive(spec)
        .map_err(PrivilegeError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalation_puts_sudo_first_and_keeps_the_original_argv() {
        let command = escalate(
            CommandSpec::new("apt-get")
                .arg("install")
                .arg("-y")
                .arg("libglib2.0-0"),
        );

        assert_eq!(
            command.display(),
            "LC_ALL=C sudo apt-get install -y libglib2.0-0"
        );
    }

    #[test]
    fn escalation_never_asks_sudo_to_read_a_password_from_stdin() {
        let command = escalate(
            CommandSpec::new("dnf")
                .arg("install")
                .arg("-y")
                .arg("glib2"),
        );

        assert!(
            !command.display().contains(" -S"),
            "sudo must not receive a stdin password flag: {}",
            command.display()
        );
        assert!(
            command.display().starts_with("LC_ALL=C sudo "),
            "the elevated command is not a separate sudo invocation: {}",
            command.display()
        );
    }

    #[test]
    fn a_non_tty_refusal_names_the_command_and_the_hint() {
        let error = PrivilegeError::NotATerminal {
            command: "LC_ALL=C sudo dnf install -y glib2".to_owned(),
        };
        let message = error.to_string();

        assert!(message.contains("sudo -v"), "{message}");
        assert!(message.contains("dnf install -y glib2"), "{message}");
    }
}
