use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::detect::{self, Family};
use crate::ui;
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
    let resolve_root = options.resolve_root;
    let _state_dir = options.state_dir;
    let _dry_run = options.dry_run;

    if !options.report {
        return ui::run(move || build_report(&os_release_path, &resolve_root));
    }

    let report = match build_report(&os_release_path, &resolve_root) {
        Ok(report) => report,
        Err(message) => {
            eprintln!("resolve-linux: {message}");
            return 1;
        }
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

fn build_report(os_release: &Path, resolve_root: &Path) -> Result<Report, String> {
    let distro = detect::detect(os_release)
        .map_err(|error| format!("cannot read {}: {error}", os_release.display()))?;

    let family = distro.family.map(Family::as_str).unwrap_or("unsupported");
    let supported = distro.supported();

    Ok(Report {
        schema_version: 1,
        distro: DistroSection {
            id: distro.id,
            name: distro.name,
            family: family.to_owned(),
            supported,
            manual_packages: distro.manual_dependencies,
        },
        package_manager: distro
            .family
            .map(|family| family.package_manager().to_owned()),
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
    })
}

#[derive(Debug, Parser)]
#[command(
    name = "resolve-linux",
    version,
    about = "Helper to install DaVinci Resolve and Resolve Studio on Linux"
)]
struct Options {
    /// Output a JSON report of system info instead of starting the terminal UI.
    #[arg(long = "report")]
    report: bool,

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
        assert!(options.report);
    }

    #[test]
    fn no_arguments_are_accepted_so_the_tui_can_start() {
        let options = parse(&[]).expect("no arguments opens the terminal UI");
        assert!(!options.report);
        assert!(!parse(&["--dry-run"]).expect("valid arguments").report);
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
        assert!(options.report);
    }

    #[test]
    fn unknown_options_and_missing_values_are_errors() {
        assert!(parse(&["--report", "--not-a-real-option"]).is_err());
        assert!(parse(&["--report", "--os-release"]).is_err());
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Report {
    pub(crate) schema_version: u32,
    pub(crate) distro: DistroSection,
    pub(crate) package_manager: Option<String>,
    pub(crate) resolve: ResolveSection,
    pub(crate) dependencies: Vec<serde_json::Value>,
    pub(crate) gpu: GpuSection,
    pub(crate) patch: PatchSection,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DistroSection {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) family: String,
    pub(crate) supported: bool,
    pub(crate) manual_packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResolveSection {
    pub(crate) installed: bool,
    pub(crate) version: Option<String>,
    pub(crate) tested: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GpuSection {
    pub(crate) vendors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PatchSection {
    pub(crate) applicable: bool,
}
