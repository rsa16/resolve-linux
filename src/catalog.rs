use std::process::Output;

use crate::detect::Family;
use crate::exec::CommandSpec;

/// A logical Dependency that DaVinci Resolve needs on Linux.
///
/// The enum contains all dependency names and order. Each
/// variant maps to one native package per distro family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dependency {
    Glib,
    LibGl,
    LibGlu,
    LibCurl,
    LibXCrypt,
    OpenCl,
    Vulkan,
}

impl Dependency {
    pub const ALL: &'static [Dependency] = &[
        Dependency::Glib,
        Dependency::LibGl,
        Dependency::LibGlu,
        Dependency::LibCurl,
        Dependency::LibXCrypt,
        Dependency::OpenCl,
        Dependency::Vulkan,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Dependency::Glib => "glib",
            Dependency::LibGl => "libgl",
            Dependency::LibGlu => "libglu",
            Dependency::LibCurl => "libcurl",
            Dependency::LibXCrypt => "libxcrypt",
            Dependency::OpenCl => "opencl",
            Dependency::Vulkan => "vulkan",
        }
    }

    fn package_name(self, family: Family) -> &'static str {
        match family {
            Family::Debian => match self {
                Dependency::Glib => "libglib2.0-0",
                Dependency::LibGl => "libgl1",
                Dependency::LibGlu => "libglu1-mesa",
                Dependency::LibCurl => "libcurl4",
                Dependency::LibXCrypt => "libcrypt1",
                Dependency::OpenCl => "ocl-icd-libopencl1",
                Dependency::Vulkan => "libvulkan1",
            },
            Family::Fedora => match self {
                Dependency::Glib => "glib2",
                Dependency::LibGl => "mesa-libGL",
                Dependency::LibGlu => "mesa-libGLU",
                Dependency::LibCurl => "libcurl",
                Dependency::LibXCrypt => "libxcrypt-compat",
                Dependency::OpenCl => "ocl-icd",
                Dependency::Vulkan => "vulkan-loader",
            },
            Family::Arch => match self {
                Dependency::Glib => "glib2",
                Dependency::LibGl => "libglvnd",
                Dependency::LibGlu => "glu",
                Dependency::LibCurl => "curl",
                Dependency::LibXCrypt => "libxcrypt-compat",
                Dependency::OpenCl => "ocl-icd",
                Dependency::Vulkan => "vulkan-icd-loader",
            },
        }
    }

    pub fn package(self, family: Family) -> &'static str {
        self.package_name(family)
    }
}

/// The Dependency Set for one supported distro family.
#[derive(Debug, Clone, Copy)]
pub struct Catalog {
    family: Family,
}

impl Catalog {
    pub fn for_family(family: Family) -> Self {
        Self { family }
    }

    pub fn dependencies(&self) -> &'static [Dependency] {
        Dependency::ALL
    }

    pub fn package(&self, dependency: Dependency) -> &'static str {
        dependency.package(self.family)
    }

    /// Generate CommandSpec to probe a dependency
    pub fn probe(&self, dependency: Dependency) -> CommandSpec {
        let package = dependency.package(self.family);
        match self.family {
            Family::Debian => CommandSpec::new("dpkg-query")
                .arg("-W")
                .arg("-f=${Status}")
                .arg(package),
            Family::Fedora => CommandSpec::new("rpm").arg("-q").arg(package),
            Family::Arch => CommandSpec::new("pacman").arg("-Q").arg(package),
        }
    }

    /// Check if probe output means Dependency is installed.
    pub fn is_present(&self, output: &Output) -> bool {
        match self.family {
            Family::Debian => {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("install ok installed")
            }
            Family::Fedora | Family::Arch => output.status.success(),
        }
    }

    /// Build the CommandSpec to install the package
    pub fn install(&self, packages: &[String]) -> CommandSpec {
        let mut spec = match self.family {
            Family::Debian => CommandSpec::new("apt-get").arg("install").arg("-y"),
            Family::Fedora => CommandSpec::new("dnf").arg("install").arg("-y"),
            Family::Arch => CommandSpec::new("pacman")
                .arg("-S")
                .arg("--needed")
                .arg("--noconfirm"),
        };
        for package in packages {
            spec = spec.arg(package);
        }
        spec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn output(status_code: i32, stdout: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(status_code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn packages(catalog: &Catalog) -> Vec<&'static str> {
        catalog
            .dependencies()
            .iter()
            .map(|dependency| catalog.package(*dependency))
            .collect()
    }

    #[test]
    fn the_dependency_set_has_the_expected_logical_names() {
        let names: Vec<&str> = Dependency::ALL
            .iter()
            .map(|dependency| dependency.name())
            .collect();
        assert_eq!(
            names,
            [
                "glib",
                "libgl",
                "libglu",
                "libcurl",
                "libxcrypt",
                "opencl",
                "vulkan"
            ]
        );
    }

    #[test]
    fn every_dependency_maps_a_package_for_every_family() {
        for family in [Family::Debian, Family::Fedora, Family::Arch] {
            let catalog = Catalog::for_family(family);
            for dependency in Dependency::ALL {
                assert!(
                    !catalog.package(*dependency).is_empty(),
                    "{family:?}/{} has no package",
                    dependency.name()
                );
                assert!(
                    !catalog.probe(*dependency).display().is_empty(),
                    "{family:?}/{} has no probe",
                    dependency.name()
                );
            }
        }
    }

    #[test]
    fn install_program_matches_the_family_package_manager() {
        for family in [Family::Debian, Family::Fedora, Family::Arch] {
            let command = Catalog::for_family(family).install(&[]).display();
            assert!(
                command.contains(&format!(" {} ", family.package_manager()))
                    || command.ends_with(&format!(" {}", family.package_manager())),
                "{family:?} install program disagrees with the package manager: {command}"
            );
        }
    }

    #[test]
    fn probe_commands_are_family_appropriate() {
        let display = |family| {
            Catalog::for_family(family)
                .probe(Dependency::Glib)
                .display()
        };

        assert_eq!(
            display(Family::Debian),
            "LC_ALL=C dpkg-query -W '-f=${Status}' libglib2.0-0"
        );
        assert_eq!(display(Family::Fedora), "LC_ALL=C rpm -q glib2");
        assert_eq!(display(Family::Arch), "LC_ALL=C pacman -Q glib2");
    }

    #[test]
    fn install_commands_are_family_appropriate() {
        let packages = vec!["glib2".to_owned(), "curl".to_owned()];

        assert_eq!(
            Catalog::for_family(Family::Debian)
                .install(&packages)
                .display(),
            "LC_ALL=C apt-get install -y glib2 curl"
        );
        assert_eq!(
            Catalog::for_family(Family::Fedora)
                .install(&packages)
                .display(),
            "LC_ALL=C dnf install -y glib2 curl"
        );
        assert_eq!(
            Catalog::for_family(Family::Arch)
                .install(&packages)
                .display(),
            "LC_ALL=C pacman -S --needed --noconfirm glib2 curl"
        );
    }

    #[test]
    fn dpkg_status_distinguishes_installed_from_removed() {
        let catalog = Catalog::for_family(Family::Debian);
        assert!(catalog.is_present(&output(0, "install ok installed\n")));
        assert!(!catalog.is_present(&output(0, "deinstall ok config-files\n")));
        assert!(!catalog.is_present(&output(1, "")));
    }

    #[test]
    fn rpm_and_pacman_use_the_exit_status() {
        for family in [Family::Fedora, Family::Arch] {
            let catalog = Catalog::for_family(family);
            assert!(catalog.is_present(&output(0, "glib2-1.0\n")));
            assert!(!catalog.is_present(&output(1, "package glib2 is not installed\n")));
        }
    }

    #[test]
    fn package_names_are_family_specific() {
        assert_eq!(
            packages(&Catalog::for_family(Family::Debian)),
            [
                "libglib2.0-0",
                "libgl1",
                "libglu1-mesa",
                "libcurl4",
                "libcrypt1",
                "ocl-icd-libopencl1",
                "libvulkan1"
            ]
        );
        assert_eq!(
            packages(&Catalog::for_family(Family::Fedora)),
            [
                "glib2",
                "mesa-libGL",
                "mesa-libGLU",
                "libcurl",
                "libxcrypt-compat",
                "ocl-icd",
                "vulkan-loader"
            ]
        );
        assert_eq!(
            packages(&Catalog::for_family(Family::Arch)),
            [
                "glib2",
                "libglvnd",
                "glu",
                "curl",
                "libxcrypt-compat",
                "ocl-icd",
                "vulkan-icd-loader"
            ]
        );
    }
}
