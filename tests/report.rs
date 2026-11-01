use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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
    let temp = TempDir::new("report-default");
    for program in ["dpkg-query", "rpm", "pacman"] {
        write_probe_stub(&temp, program);
    }
    let log = temp.path().join("commands.log");
    Command::new(BIN)
        .arg("--report")
        .arg("--os-release")
        .arg(os_release)
        .arg("--resolve-root")
        .arg("/nonexistent/resolve")
        .env("PATH", temp.path())
        .env("RESOLVE_LINUX_COMMAND_LOG", &log)
        .env("RESOLVE_LINUX_PRESENT", "")
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

/// A probe stub that records its argv and LC_ALL, reports the packages named
/// in `RESOLVE_LINUX_PRESENT` as installed, and everything else as missing.
fn write_probe_stub(temp: &TempDir, program: &str) {
    let stub = temp.path().join(program);
    fs::write(
        &stub,
        "#!/bin/sh\n\
         {\n\
           printf 'command=%s' \"$0\"\n\
           for arg in \"$@\"; do printf ' %s' \"$arg\"; done\n\
           printf '\\nLC_ALL=%s\\n' \"$LC_ALL\"\n\
         } >> \"$RESOLVE_LINUX_COMMAND_LOG\"\n\
         last=\n\
         for arg in \"$@\"; do last=$arg; done\n\
         case \",$RESOLVE_LINUX_PRESENT,\" in\n\
           *\",$last,\"*) present=1 ;;\n\
           *) present=0 ;;\n\
         esac\n\
         if [ \"$present\" = 1 ]; then\n\
           case \"$0\" in\n\
             *dpkg-query) printf 'install ok installed\\n' ;;\n\
           esac\n\
           exit 0\n\
         fi\n\
         exit 1\n",
    )
    .expect("write probe stub");
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))
        .expect("make probe stub executable");
}

fn run_with_stubs(temp: &TempDir, present: &[&str], args: &[&str]) -> (Output, PathBuf) {
    let log = temp.path().join("commands.log");
    let output = Command::new(BIN)
        .args(args)
        .env("PATH", temp.path())
        .env("RESOLVE_LINUX_COMMAND_LOG", &log)
        .env("RESOLVE_LINUX_SUDO_LOG", temp.path().join("sudo.log"))
        .env("RESOLVE_LINUX_PRESENT", present.join(","))
        .output()
        .expect("run resolve-linux");
    (output, log)
}

fn command_log(log: &Path) -> String {
    fs::read_to_string(log).unwrap_or_default()
}

fn write_recording_stub(stub: &Path, log_env: &str, delegate: bool) {
    let mut body = format!(
        "#!/bin/sh\n\
         {{ printf '%s' \"$0\"; for arg in \"$@\"; do printf ' %s' \"$arg\"; done; printf '\\n'; }} >> \"${log_env}\"\n"
    );
    if delegate {
        body.push_str("exec \"$@\"\n");
    }
    fs::write(stub, body).expect("write recording stub");
    fs::set_permissions(stub, fs::Permissions::from_mode(0o755))
        .expect("make recording stub executable");
}

fn run_with_pty(args: &[&str], envs: &[(&str, &str)]) -> Output {
    use std::io::Read;

    let pty = nix::pty::openpty(None, None).expect("open pseudo-terminal");
    let slave = fs::File::from(pty.slave);

    let mut command = Command::new(BIN);
    command
        .args(args)
        .stdin(Stdio::from(slave.try_clone().expect("clone pty slave")))
        .stdout(Stdio::from(slave.try_clone().expect("clone pty slave")))
        .stderr(Stdio::from(slave.try_clone().expect("clone pty slave")));
    for (key, value) in envs {
        command.env(key, value);
    }

    let mut child = command.spawn().expect("spawn resolve-linux on a pty");
    drop(command);
    drop(slave);

    let status = child.wait().expect("wait for resolve-linux");
    let mut output = Vec::new();
    let mut master = fs::File::from(pty.master);
    let _ = master.read_to_end(&mut output);

    Output {
        status,
        stdout: output,
        stderr: Vec::new(),
    }
}

#[test]
fn report_probes_every_dependency_per_family() {
    struct Case {
        fixture: &'static str,
        probe: &'static str,
        expected: &'static [(&'static str, &'static str)],
        present: &'static [&'static str],
    }

    let cases = [
        Case {
            fixture: "ubuntu-24.04",
            probe: "dpkg-query",
            expected: &[
                ("glib", "libglib2.0-0"),
                ("libgl", "libgl1"),
                ("libglu", "libglu1-mesa"),
                ("libcurl", "libcurl4"),
                ("libxcrypt", "libcrypt1"),
                ("opencl", "ocl-icd-libopencl1"),
                ("vulkan", "libvulkan1"),
            ],
            present: &["libglib2.0-0", "libcurl4"],
        },
        Case {
            fixture: "fedora-41",
            probe: "rpm",
            expected: &[
                ("glib", "glib2"),
                ("libgl", "mesa-libGL"),
                ("libglu", "mesa-libGLU"),
                ("libcurl", "libcurl"),
                ("libxcrypt", "libxcrypt-compat"),
                ("opencl", "ocl-icd"),
                ("vulkan", "vulkan-loader"),
            ],
            present: &["glib2"],
        },
        Case {
            fixture: "arch",
            probe: "pacman",
            expected: &[
                ("glib", "glib2"),
                ("libgl", "libglvnd"),
                ("libglu", "glu"),
                ("libcurl", "curl"),
                ("libxcrypt", "libxcrypt-compat"),
                ("opencl", "ocl-icd"),
                ("vulkan", "vulkan-icd-loader"),
            ],
            present: &["vulkan-icd-loader"],
        },
    ];

    for case in cases {
        let temp = TempDir::new(&format!("probe-{}", case.fixture));
        write_probe_stub(&temp, case.probe);
        let (output, log) = run_with_stubs(
            &temp,
            case.present,
            &[
                "--report",
                "--os-release",
                fixture(case.fixture)
                    .to_str()
                    .expect("fixture path is UTF-8"),
                "--resolve-root",
                "/nonexistent/resolve",
            ],
        );
        let report = json_stdout(&output);

        let entries = report["dependencies"]
            .as_array()
            .expect("dependencies is an array");
        assert_eq!(
            entries.len(),
            case.expected.len(),
            "wrong dependency count for {}: {report}",
            case.fixture
        );

        for (logical, package) in case.expected {
            let entry = entries
                .iter()
                .find(|entry| entry["name"] == *logical)
                .unwrap_or_else(|| panic!("{} is missing from {report}", logical));
            assert_eq!(entry["package"], *package, "wrong package for {logical}");
            let present = entry["present"]
                .as_bool()
                .unwrap_or_else(|| panic!("{logical} has no boolean present in {report}"));
            assert_eq!(
                present,
                case.present.contains(package),
                "wrong present state for {logical} on {}",
                case.fixture
            );
        }

        let recorded = command_log(&log);
        for (_, package) in case.expected {
            let expected_argv = match case.probe {
                "dpkg-query" => format!("dpkg-query -W -f=${{Status}} {package}"),
                "rpm" => format!("rpm -q {package}"),
                "pacman" => format!("pacman -Q {package}"),
                other => unreachable!("unexpected probe {other}"),
            };
            assert!(
                recorded.contains(&expected_argv),
                "missing probe `{expected_argv}` for {}:\n{recorded}",
                case.fixture
            );
        }
        assert!(
            recorded
                .lines()
                .filter(|line| line.starts_with("LC_ALL="))
                .all(|line| line == "LC_ALL=C"),
            "probes must run under LC_ALL=C:\n{recorded}"
        );
    }
}

#[test]
fn install_deps_on_unsupported_distribution_refuses_and_names_the_id() {
    let temp = TempDir::new("install-unsupported");
    let (output, log) = run_with_stubs(
        &temp,
        &[],
        &[
            "--dry-run",
            "--install-deps",
            "--os-release",
            fixture("opensuse-tumbleweed")
                .to_str()
                .expect("fixture path is UTF-8"),
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("opensuse-tumbleweed"),
        "refusal must name the detected ID: {stderr}"
    );
    assert!(
        !log.exists(),
        "unsupported install-deps ran commands: {}",
        command_log(&log)
    );
}

#[test]
fn unsupported_distribution_has_no_dependency_entries() {
    let temp = TempDir::new("unsupported-deps");
    write_probe_stub(&temp, "rpm");
    let (output, _) = run_with_stubs(
        &temp,
        &[],
        &[
            "--report",
            "--os-release",
            fixture("opensuse-tumbleweed")
                .to_str()
                .expect("fixture path is UTF-8"),
            "--resolve-root",
            "/nonexistent/resolve",
        ],
    );
    let report = json_stdout(&output);
    assert!(
        report["dependencies"]
            .as_array()
            .expect("dependencies is an array")
            .is_empty(),
        "an unsupported distro must not claim a Dependency Set: {report}"
    );
}

#[test]
fn dry_run_report_runs_only_read_only_probes() {
    // Probes are read-only and always run; --dry-run only suppresses commands
    // that would mutate the machine.
    let temp = TempDir::new("stub-path");
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
        write_probe_stub(&temp, program);
    }
    let (output, log) = run_with_stubs(
        &temp,
        &["glib2"],
        &[
            "--report",
            "--dry-run",
            "--os-release",
            fixture("fedora-41")
                .to_str()
                .expect("fixture path is UTF-8"),
            "--resolve-root",
            "/nonexistent/resolve",
        ],
    );

    assert!(output.status.success(), "report should still succeed");
    let recorded = command_log(&log);
    assert!(
        recorded.contains("rpm -q glib2"),
        "probes did not run:\n{recorded}"
    );
    for forbidden in ["dnf ", "apt-get ", "sudo ", " -S "] {
        assert!(
            !recorded.contains(forbidden),
            "--dry-run ran a mutating command containing `{forbidden}`:\n{recorded}"
        );
    }
}

#[test]
fn dry_run_install_deps_prints_the_install_template_without_running_it() {
    let cases = [
        (
            "ubuntu-24.04",
            "dpkg-query",
            "apt-get install -y libglib2.0-0 libgl1 libglu1-mesa libcurl4 libcrypt1 ocl-icd-libopencl1 libvulkan1",
        ),
        (
            "fedora-41",
            "rpm",
            "dnf install -y glib2 mesa-libGL mesa-libGLU libcurl libxcrypt-compat ocl-icd vulkan-loader",
        ),
        (
            "arch",
            "pacman",
            "pacman -S --needed --noconfirm glib2 libglvnd glu curl libxcrypt-compat ocl-icd vulkan-icd-loader",
        ),
    ];

    for (fixture_name, probe, expected) in cases {
        let temp = TempDir::new(&format!("install-{}", fixture_name));
        write_probe_stub(&temp, probe);
        // The package manager is a stub too, but it must never run.
        write_probe_stub(
            &temp,
            match probe {
                "dpkg-query" => "apt-get",
                "rpm" => "dnf",
                _ => "pacman",
            },
        );
        write_probe_stub(&temp, "sudo");

        let (output, log) = run_with_stubs(
            &temp,
            &[],
            &[
                "--dry-run",
                "--install-deps",
                "--os-release",
                fixture(fixture_name)
                    .to_str()
                    .expect("fixture path is UTF-8"),
            ],
        );
        assert!(
            output.status.success(),
            "{fixture_name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let expected = format!("would run: LC_ALL=C sudo {expected}");
        assert!(
            stdout.lines().any(|line| line.trim() == expected),
            "{fixture_name}: elevated install argv missing\nstdout: {stdout}"
        );

        let recorded = command_log(&log);
        let mutating = match probe {
            "dpkg-query" => "apt-get install",
            "rpm" => "dnf install",
            _ => "pacman -S",
        };
        assert!(
            !recorded.contains(mutating),
            "{fixture_name}: --dry-run ran `{mutating}`:\n{recorded}"
        );
        assert!(
            !recorded.contains("/sudo "),
            "{fixture_name}: --dry-run ran sudo:\n{recorded}"
        );
    }
}

#[test]
fn install_deps_without_a_tty_refuses_before_running_any_install() {
    let temp = TempDir::new("install-no-tty");
    write_probe_stub(&temp, "rpm");
    write_recording_stub(&temp.path().join("sudo"), "RESOLVE_LINUX_SUDO_LOG", false);
    write_recording_stub(&temp.path().join("dnf"), "RESOLVE_LINUX_COMMAND_LOG", false);
    let sudo_log = temp.path().join("sudo.log");
    let command_log_path = temp.path().join("commands.log");
    let (output, _) = run_with_stubs(
        &temp,
        &[],
        &[
            "--install-deps",
            "--os-release",
            fixture("fedora-41")
                .to_str()
                .expect("fixture path is UTF-8"),
        ],
    );

    assert!(!output.status.success(), "a TTY-less install must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sudo -v"),
        "refusal must instruct the user to prime sudo: {stderr}"
    );
    assert!(
        stderr.contains("dnf install -y"),
        "refusal must print the exact command for manual use: {stderr}"
    );
    assert!(
        !command_log(&command_log_path).contains("install -y"),
        "no package-manager install may run without a TTY:\n{}",
        command_log(&command_log_path)
    );
    assert!(
        !sudo_log.exists(),
        "sudo was invoked without a terminal: {}",
        command_log(&sudo_log)
    );
}

#[test]
fn install_deps_on_a_tty_elevates_with_sudo_and_installs_only_missing_packages() {
    let temp = TempDir::new("install-real");
    write_probe_stub(&temp, "dpkg-query");
    write_recording_stub(
        &temp.path().join("apt-get"),
        "RESOLVE_LINUX_INSTALL_LOG",
        false,
    );
    write_recording_stub(&temp.path().join("sudo"), "RESOLVE_LINUX_SUDO_LOG", true);

    let command_log_path = temp.path().join("commands.log");
    let install_log = temp.path().join("install.log");
    let sudo_log = temp.path().join("sudo.log");

    let output = run_with_pty(
        &[
            "--install-deps",
            "--os-release",
            fixture("ubuntu-24.04")
                .to_str()
                .expect("fixture path is UTF-8"),
        ],
        &[
            ("PATH", temp.path().to_str().expect("temp path is UTF-8")),
            (
                "RESOLVE_LINUX_COMMAND_LOG",
                command_log_path.to_str().expect("temp path is UTF-8"),
            ),
            (
                "RESOLVE_LINUX_INSTALL_LOG",
                install_log.to_str().expect("temp path is UTF-8"),
            ),
            (
                "RESOLVE_LINUX_SUDO_LOG",
                sudo_log.to_str().expect("temp path is UTF-8"),
            ),
            ("RESOLVE_LINUX_PRESENT", "libglib2.0-0,libcurl4"),
        ],
    );

    assert!(
        output.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let expected = "apt-get install -y libgl1 libglu1-mesa libcrypt1 ocl-icd-libopencl1 libvulkan1";
    let recorded = command_log(&sudo_log);
    assert!(
        recorded.lines().any(|line| line.ends_with(expected)),
        "sudo did not receive only the missing packages:\n{recorded}"
    );
    assert!(
        !recorded.contains("libglib2.0-0") && !recorded.contains("libcurl4"),
        "packages already present were installed again:\n{recorded}"
    );
    assert!(
        command_log(&install_log).contains(expected),
        "sudo did not start the family package manager:\n{}",
        command_log(&install_log)
    );
    assert!(
        !command_log(&command_log_path).contains("apt-get install"),
        "the install ran outside the sudo child:\n{}",
        command_log(&command_log_path)
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
