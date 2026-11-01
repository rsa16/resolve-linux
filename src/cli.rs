use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::catalog::{Catalog, Dependency};
use crate::detect::{self, Family};
use crate::exec::Runner;
use crate::privilege;
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
    let dry_run = options.dry_run;

    if options.install_deps {
        return match install_deps(&os_release_path, dry_run) {
            Ok(()) => 0,
            Err(message) => {
                eprintln!("resolve-linux: {message}");
                1
            }
        };
    }

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
    let (dependencies, probe_log) = distro.family.map(probe_dependencies).unwrap_or_default();

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
        dependencies,
        gpu: GpuSection {
            vendors: Vec::new(),
        },
        patch: PatchSection { applicable: false },
        probe_log,
    })
}

/// Probe every Dependency in the family's catalog. Note that because probes
/// are read-only, they still run under `--dry-run`.
fn probe_dependencies(family: Family) -> (Vec<DependencySection>, Vec<ProbeRecord>) {
    let catalog = Catalog::for_family(family);
    let runner = Runner::new(false);
    let mut dependencies = Vec::new();
    let mut probe_log = Vec::new();

    for dependency in catalog.dependencies() {
        let dependency = *dependency;
        let mut present = false;

        let probe_command = catalog.probe(dependency);
        let output = runner.run(&probe_command).ok().flatten();

        if let Some(output) = &output {
            present = catalog.is_present(output);
        }

        probe_log.push(ProbeRecord {
            command: probe_command.display(),
            exit_code: output.as_ref().and_then(|output| output.status.code()),
        });

        dependencies.push(DependencySection {
            name: dependency.name().to_owned(),
            package: catalog.package(dependency).to_owned(),
            present,
        });
    }

    (dependencies, probe_log)
}

fn probe_present(runner: &Runner, catalog: &Catalog, dependency: Dependency) -> bool {
    runner
        .run(&catalog.probe(dependency))
        .ok()
        .flatten()
        .is_some_and(|output| catalog.is_present(&output))
}

/// Install the missing packages for detected family.
fn install_deps(os_release: &Path, dry_run: bool) -> Result<(), String> {
    let distro = detect::detect(os_release)
        .map_err(|error| format!("cannot read {}: {error}", os_release.display()))?;
    let Some(family) = distro.family else {
        return Err(format!(
            "unsupported distribution `{}`; install these packages manually: {}",
            distro.id,
            distro.manual_dependencies.join(", ")
        ));
    };

    let catalog = Catalog::for_family(family);
    let runner = Runner::new(false);
    let missing: Vec<String> = catalog
        .dependencies()
        .iter()
        .copied()
        .filter(|dependency| !probe_present(&runner, &catalog, *dependency))
        .map(|dependency| catalog.package(dependency).to_owned())
        .collect();

    if missing.is_empty() {
        println!("All dependencies are present.");
        return Ok(());
    }

    let command = privilege::escalate(catalog.install(&missing));

    if dry_run {
        Runner::new(true)
            .run(&command)
            .map_err(|error| format!("cannot print the install command: {error}"))?;
        return Ok(());
    }

    let status = privilege::run(&command).map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("dependency install failed ({status})"))
    }
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
    /// Read-only probes still run.
    #[arg(long)]
    dry_run: bool,

    /// Install the missing dependencies. Will prompt for sudo.
    #[arg(long, conflicts_with = "report")]
    install_deps: bool,

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
        assert!(!options.install_deps);
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
    fn install_deps_is_a_non_report_mode() {
        let options = parse(&["--dry-run", "--install-deps"]).expect("valid arguments");
        assert!(options.install_deps);
        assert!(!options.report);
        assert!(parse(&["--report", "--install-deps"]).is_err());
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
    pub(crate) dependencies: Vec<DependencySection>,
    pub(crate) gpu: GpuSection,
    pub(crate) patch: PatchSection,
    #[serde(skip)]
    pub(crate) probe_log: Vec<ProbeRecord>,
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
pub(crate) struct DependencySection {
    pub(crate) name: String,
    pub(crate) package: String,
    pub(crate) present: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ProbeRecord {
    pub(crate) command: String,
    pub(crate) exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GpuSection {
    pub(crate) vendors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PatchSection {
    pub(crate) applicable: bool,
}
