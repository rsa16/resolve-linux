use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::detect::{self, Family};
use clap::Parser;
use nix::unistd::geteuid;
use serde::Serialize;

const ROOT_REFUSAL: &str = "resolve-linux cannot be run as root.\n\
Run it as your normal user; privileged operations will prompt for sudo.";

pub fn run<I>(args: I) -> i32
where
    I: IntoIterator<Item = OsString>,
{
    if geteuid().as_raw() == 0 {
        eprintln!("{ROOT_REFUSAL}");
        return 2;
    }

    let options = match Options::try_parse_from(args) {
        Ok(options) => options,
        Err(error) => {
            let _ = error.print();
            return error.exit_code();
        }
    };

    let os_release_path = options.os_release.unwrap_or_else(detect::default_path);
    let distro = match detect::detect(&os_release_path) {
        Ok(distro) => distro,
        Err(error) => {
            eprintln!(
                "resolve-linux: cannot read {}: {error}",
                os_release_path.display()
            );
            return 1;
        }
    };

    let resolve_root = options.resolve_root;
    let _state_dir = options.state_dir;
    let _dry_run = options.dry_run;

    let report = Report {
        schema_version: 1,
        distro: DistroSection {
            id: &distro.id,
            name: &distro.name,
            family: distro.family.map(Family::as_str).unwrap_or("unsupported"),
            supported: distro.supported(),
            manual_packages: &distro.manual_dependencies,
        },
        package_manager: distro.family.map(Family::package_manager),
        resolve: ResolveSection {
            installed: resolve_root.is_dir(),
            version: None,
            tested: false,
        },
        dependencies: Vec::new(),
        gpu: GpuSection {
            vendors: Vec::new(),
        },
        patch: PatchSection { applicable: false },
    };

    let json = match serde_json::to_string(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("resolve-linux: cannot serialise report: {error}");
            return 1;
        }
    };

    let mut stdout = io::stdout().lock();
    if let Err(error) = writeln!(stdout, "{json}").and_then(|()| stdout.flush()) {
        eprintln!("resolve-linux: cannot write report: {error}");
        return 1;
    }
    0
}

#[derive(Debug, Parser)]
#[command(
    name = "resolve-linux",
    version,
    about = "Helper to install DaVinci Resolve and Resolve Studio on Linux"
)]
struct Options {
    /// Output a JSON report of system info.
    #[arg(long = "report", required = true)]
    _report: bool,

    /// Show commands that would be executed without actually running them.
    #[arg(long)]
    dry_run: bool,

    /// Use this os-release file instead of the system default; generally not recommended.
    #[arg(long, value_name = "PATH")]
    os_release: Option<PathBuf>,

    /// Set the DaVinci Resolve installation directory.
    #[arg(long, value_name = "PATH", default_value = "/opt/resolve")]
    resolve_root: PathBuf,

    /// Set the directory used to store persistent application state.
    #[arg(long, value_name = "PATH")]
    state_dir: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, clap::Error> {
        Options::try_parse_from(std::iter::once("resolve-linux").chain(args.iter().copied()))
    }

    #[test]
    fn defaults_are_applied_when_only_report_is_given() {
        let options = parse(&["--report"]).expect("valid arguments");
        assert_eq!(options.resolve_root, PathBuf::from("/opt/resolve"));
        assert!(options.os_release.is_none());
        assert!(options.state_dir.is_none());
        assert!(!options.dry_run);
    }

    #[test]
    fn report_is_required() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--dry-run"]).is_err());
    }

    #[test]
    fn all_flags_are_accepted_in_any_order() {
        let options = parse(&[
            "--dry-run",
            "--state-dir",
            "/tmp/state",
            "--report",
            "--resolve-root",
            "/tmp/resolve",
            "--os-release",
            "/tmp/os-release",
        ])
        .expect("valid arguments");
        assert_eq!(options.os_release, Some(PathBuf::from("/tmp/os-release")));
        assert_eq!(options.resolve_root, PathBuf::from("/tmp/resolve"));
        assert_eq!(options.state_dir, Some(PathBuf::from("/tmp/state")));
        assert!(options.dry_run);
    }

    #[test]
    fn unknown_options_and_missing_values_are_errors() {
        assert!(parse(&["--report", "--not-a-real-option"]).is_err());
        assert!(parse(&["--report", "--os-release"]).is_err());
    }
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u32,
    distro: DistroSection<'a>,
    package_manager: Option<&'static str>,
    resolve: ResolveSection,
    dependencies: Vec<serde_json::Value>,
    gpu: GpuSection,
    patch: PatchSection,
}

#[derive(Serialize)]
struct DistroSection<'a> {
    id: &'a str,
    name: &'a str,
    family: &'static str,
    supported: bool,
    manual_packages: &'a [String],
}

#[derive(Serialize)]
struct ResolveSection {
    installed: bool,
    version: Option<String>,
    tested: bool,
}

#[derive(Serialize)]
struct GpuSection {
    vendors: Vec<String>,
}

#[derive(Serialize)]
struct PatchSection {
    applicable: bool,
}
