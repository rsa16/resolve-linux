#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Command, Output};

/// Commands to run are expressed as a program plus its explicit arguments.
///
/// `display()` is for user only. We do not build a shell string
/// since shell parsing enables shell injection; arguments are passed to the
/// child process as argv entries instead.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

impl CommandSpec {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        for (key, value) in &self.env {
            if key == "LC_ALL" {
                continue;
            }
            parts.push(format!("{}={}", quote(key), quote(value)));
        }

        parts.push("LC_ALL=C".to_owned());
        parts.push(quote(&self.program));
        parts.extend(self.args.iter().map(|arg| quote(arg)));
        parts.join(" ")
    }
}

pub struct Runner {
    dry_run: bool,
}

impl Runner {
    pub fn new(dry_run: bool) -> Self {
        Self { dry_run }
    }

    pub fn run(&self, spec: &CommandSpec) -> io::Result<Option<Output>> {
        if self.dry_run {
            println!("would run: {}", spec.display());
            return Ok(None);
        }

        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        for (key, value) in &spec.env {
            command.env(key, value);
        }

        command.env("LC_ALL", "C");
        command.output().map(Some)
    }
}

fn quote(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    let shell_safe = text
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_@%+=:,./-".contains(character));
    if shell_safe && !text.is_empty() {
        text.into_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "resolve-linux-exec-{label}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn stub(temp: &TempDir) -> PathBuf {
        let path = temp.path().join("stub");
        fs::write(
            &path,
            "#!/bin/sh\n{\n  printf 'LC_ALL=%s\\n' \"$LC_ALL\"\n  for arg in \"$@\"; do\n    printf 'arg=%s\\n' \"$arg\"\n  done\n} > \"$LOG\"\n",
        )
        .expect("write stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("make stub executable");
        path
    }

    #[test]
    fn dry_run_prints_and_does_not_execute() {
        let temp = TempDir::new("dry");
        let marker = temp.path().join("ran");
        let spec = CommandSpec::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf ran > {}", marker.display()));

        let result = Runner::new(true)
            .run(&spec)
            .expect("dry-run never fails to start a process");

        assert!(result.is_none());
        assert!(!marker.exists(), "dry-run must not start the command");
    }

    #[test]
    fn runs_argv_with_lc_all_c_and_no_shell_interpolation() {
        let temp = TempDir::new("argv");
        let log = temp.path().join("log");
        let marker = temp.path().join("injected");
        let metacharacter_arg = format!("; touch {}", marker.display());

        let spec = CommandSpec::new(stub(&temp))
            .arg("two words")
            .arg(&metacharacter_arg)
            .env("LOG", &log)
            .env("LC_ALL", "fr_FR.UTF-8");

        let output = Runner::new(false)
            .run(&spec)
            .expect("run stub")
            .expect("not a dry run");
        assert!(output.status.success(), "stub failed: {output:?}");

        let recorded = fs::read_to_string(&log).expect("stub wrote its log");
        assert!(recorded.contains("LC_ALL=C"), "LC_ALL not set: {recorded}");
        assert!(
            recorded.contains("arg=two words"),
            "argument with spaces was split: {recorded}"
        );
        assert!(
            recorded.contains(&format!("arg={metacharacter_arg}")),
            "argument was interpreted by a shell: {recorded}"
        );
        assert!(
            !marker.exists(),
            "shell metacharacters in an argument were interpreted"
        );
    }

    #[test]
    fn display_shows_environment_and_quotes_metacharacters() {
        let spec = CommandSpec::new("/usr/bin/Resolve.run")
            .arg("-x")
            .arg("two words")
            .arg("$(touch /tmp/not-run)")
            .env("SKIP_PACKAGE_CHECK", "1")
            .env("LC_ALL", "fr_FR.UTF-8");

        let display = spec.display();
        assert!(display.contains("SKIP_PACKAGE_CHECK=1"), "{display}");
        assert!(display.contains("LC_ALL=C"), "{display}");
        assert!(!display.contains("fr_FR"), "{display}");
        assert!(display.contains("'two words'"), "{display}");
        assert!(display.contains("'$(touch /tmp/not-run)'"), "{display}");
    }
}
