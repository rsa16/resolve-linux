use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_resolve-linux");
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "resolve-linux-{label}-{}-{unique}",
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

fn fixture(name: &str) -> PathBuf {
    Path::new(MANIFEST_DIR)
        .join("tests/fixtures")
        .join(name)
        .join("os-release")
}

fn run_report(os_release: &Path) -> Output {
    Command::new(BIN)
        .arg("--report")
        .arg("--os-release")
        .arg(os_release)
        .arg("--resolve-root")
        .arg("/nonexistent/resolve")
        .env("LC_ALL", "C")
        .output()
        .expect("run resolve-linux")
}

fn json_stdout(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "expected success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8");
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout is not one JSON object: {error}\nstdout: {stdout}"))
}

#[test]
fn report_contains_the_required_sections() {
    let output = run_report(&fixture("fedora-41"));
    let report = json_stdout(&output);

    assert_eq!(report["schema_version"], 1);
    for section in ["distro", "resolve", "dependencies", "gpu", "patch"] {
        assert!(
            !report[section].is_null(),
            "report is missing the {section} section: {report}"
        );
    }
    assert_eq!(report["resolve"]["installed"], false);
    assert!(report["dependencies"].is_array());
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1,
        "report must be one JSON object followed by a single newline"
    );
}

#[test]
fn family_and_package_manager_are_detected_per_fixture() {
    let expected: BTreeMap<&str, (&str, Option<&str>)> = BTreeMap::from([
        ("arch", ("arch", Some("pacman"))),
        ("fedora-41", ("fedora", Some("dnf"))),
        ("manjaro", ("arch", Some("pacman"))),
        ("nobara", ("fedora", Some("dnf"))),
        ("opensuse-tumbleweed", ("unsupported", None)),
        ("pop-os", ("debian", Some("apt-get"))),
        ("ubuntu-24.04", ("debian", Some("apt-get"))),
    ]);

    let fixtures_dir = Path::new(MANIFEST_DIR).join("tests/fixtures");
    let mut discovered = BTreeSet::new();
    for entry in fs::read_dir(&fixtures_dir).expect("read fixtures directory") {
        let entry = entry.expect("read fixture entry");
        if entry.file_type().expect("fixture file type").is_dir() {
            discovered.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }
    let expected_names: BTreeSet<String> = expected.keys().map(|name| (*name).to_owned()).collect();
    assert_eq!(
        discovered, expected_names,
        "every fixture directory needs an expected family/package-manager row"
    );

    for (name, (family, package_manager)) in expected {
        let report = json_stdout(&run_report(&fixture(name)));
        assert_eq!(
            report["distro"]["family"], family,
            "wrong family for {name}"
        );

        match package_manager {
            Some(package_manager) => {
                assert_eq!(
                    report["package_manager"], package_manager,
                    "wrong package manager for {name}"
                );
                assert_eq!(
                    report["distro"]["supported"], true,
                    "{name} should be supported"
                );
                assert!(
                    report["distro"]["manual_packages"]
                        .as_array()
                        .expect("manual_packages is an array")
                        .is_empty(),
                    "{name} should not need a manual package list"
                );
            }
            None => {
                assert!(
                    report["package_manager"].is_null(),
                    "{name} must not name a package manager"
                );
                assert_eq!(
                    report["distro"]["supported"], false,
                    "{name} should be unsupported"
                );
                assert!(
                    !report["distro"]["manual_packages"]
                        .as_array()
                        .expect("manual_packages is an array")
                        .is_empty(),
                    "{name} must name manual dependencies: {report}"
                );
            }
        }
    }
}

#[test]
fn dry_run_executes_no_external_command() {
    // Report mode has no command to run in the walking skeleton. This guards the
    // invariant that any command added later must honour --dry-run; the Runner
    // unit test proves dry-run mode never starts a process.
    let temp = TempDir::new("stub-path");
    let log = temp.path().join("commands.log");
    for program in [
        "sh",
        "sudo",
        "dnf",
        "apt-get",
        "pacman",
        "dpkg-query",
        "rpm",
        "lspci",
    ] {
        let stub = temp.path().join(program);
        fs::write(
            &stub,
            "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" >> \"$RESOLVE_LINUX_COMMAND_LOG\"\n",
        )
        .expect("write stub");
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))
            .expect("make stub executable");
    }

    let output = Command::new(BIN)
        .arg("--report")
        .arg("--dry-run")
        .arg("--os-release")
        .arg(fixture("fedora-41"))
        .arg("--resolve-root")
        .arg("/nonexistent/resolve")
        .env("PATH", temp.path())
        .env("RESOLVE_LINUX_COMMAND_LOG", &log)
        .output()
        .expect("run resolve-linux");

    assert!(output.status.success(), "report should still succeed");
    assert!(
        !log.exists(),
        "--dry-run executed commands: {}",
        fs::read_to_string(&log).unwrap_or_default()
    );
}

#[test]
fn effective_uid_zero_is_refused_before_detection() {
    let probe = match Command::new("unshare").args(["-r", "true"]).status() {
        Ok(status) => status,
        Err(error) => {
            eprintln!("skipping: unshare is unavailable, cannot simulate root: {error}");
            return;
        }
    };
    if !probe.success() {
        eprintln!("skipping: user namespaces are unavailable, cannot simulate root");
        return;
    }

    let output = Command::new("unshare")
        .arg("-r")
        .arg(BIN)
        .arg("--report")
        .arg("--os-release")
        .arg("/nonexistent/os-release")
        .arg("--resolve-root")
        .arg("/nonexistent/resolve")
        .output()
        .expect("run resolve-linux as simulated root");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "root run must fail, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("root") && stderr.contains("normal user"),
        "refusal should name root and the correct way to run: {stderr}"
    );
    assert!(
        !stderr.contains("os-release") && !stderr.contains("/nonexistent/os-release"),
        "detection must not start before the root refusal: {stderr}"
    );
}

#[test]
fn normal_user_control_run_succeeds() {
    let output = run_report(&fixture("fedora-41"));
    assert!(
        output.status.success(),
        "normal-user run should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unknown_option_fails_with_usage() {
    let output = Command::new(BIN)
        .arg("--not-a-real-option")
        .output()
        .expect("run resolve-linux");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("usage"),
        "expected usage text: {stderr}"
    );
}

#[test]
fn os_release_without_id_is_an_error() {
    let temp = TempDir::new("missing-id");
    let os_release = temp.path().join("os-release");
    fs::write(&os_release, "NAME='Not an os-release'\n").expect("write os-release");

    let output = run_report(&os_release);
    assert!(
        !output.status.success(),
        "a file without ID must not be reported as an unsupported distro"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ID"),
        "error should name the missing ID: {stderr}"
    );
}

#[test]
fn os_release_values_may_be_single_quoted() {
    let temp = TempDir::new("quoted");
    let os_release = temp.path().join("os-release");
    let mut content = String::from("NAME='Quoted Linux'\nID='ubuntu'\nID_LIKE='debian'\n");
    content.push_str("VERSION_ID='24.04'\n");
    fs::write(&os_release, content).expect("write os-release");

    let output = run_report(&os_release);
    let report = json_stdout(&output);
    assert_eq!(report["distro"]["family"], "debian");
    assert_eq!(report["package_manager"], "apt-get");
}
