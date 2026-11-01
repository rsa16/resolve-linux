use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::catalog::Dependency;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Debian,
    Fedora,
    Arch,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Debian => "debian",
            Family::Fedora => "fedora",
            Family::Arch => "arch",
        }
    }

    pub fn package_manager(self) -> &'static str {
        match self {
            Family::Debian => "apt-get",
            Family::Fedora => "dnf",
            Family::Arch => "pacman",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "debian" => Some(Family::Debian),
            "fedora" => Some(Family::Fedora),
            "arch" => Some(Family::Arch),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro {
    pub id: String,
    pub name: String,
    pub family: Option<Family>,
    pub manual_dependencies: Vec<String>,
}

impl Distro {
    pub fn supported(&self) -> bool {
        self.family.is_some()
    }
}

pub fn default_path() -> PathBuf {
    let etc = PathBuf::from("/etc/os-release");
    if etc.is_file() {
        etc
    } else {
        PathBuf::from("/usr/lib/os-release")
    }
}

pub fn detect(path: &Path) -> io::Result<Distro> {
    let contents = fs::read_to_string(path)?;

    parse(&contents).map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))
}

pub fn parse(contents: &str) -> Result<Distro, String> {
    let fields = parse_fields(contents);

    let id = fields
        .get("ID")
        .map(String::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "os-release has no ID".to_owned())?
        .to_owned();

    let name = fields
        .get("NAME")
        .or_else(|| fields.get("PRETTY_NAME"))
        .map(String::as_str)
        .unwrap_or(&id)
        .to_owned();

    let family = family_from_id(&id).or_else(|| family_from_id_like(&fields));
    let manual_dependencies = if family.is_some() {
        Vec::new()
    } else {
        Dependency::ALL
            .iter()
            .map(|dependency| dependency.name().to_owned())
            .collect()
    };

    Ok(Distro {
        id,
        name,
        family,
        manual_dependencies,
    })
}

fn parse_fields(contents: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        fields.insert(key.to_owned(), unquote(value.trim()).to_owned());
    }
    fields
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn family_from_id(id: &str) -> Option<Family> {
    match id.to_ascii_lowercase().as_str() {
        "debian" | "ubuntu" => Some(Family::Debian),
        "fedora" => Some(Family::Fedora),
        "arch" => Some(Family::Arch),
        _ => None,
    }
}

fn family_from_id_like(fields: &BTreeMap<String, String>) -> Option<Family> {
    fields
        .get("ID_LIKE")?
        .split_whitespace()
        .find_map(family_from_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_like_resolves_derivatives_when_the_id_is_unknown() {
        let distro =
            parse("NAME=\"Nobara\"\nID=nobara\nID_LIKE=\"fedora\"\n").expect("valid os-release");
        assert_eq!(distro.family, Some(Family::Fedora));
        assert!(distro.supported());
    }

    #[test]
    fn id_wins_over_a_conflicting_id_like() {
        let distro = parse("ID=arch\nID_LIKE=debian\n").expect("valid os-release");
        assert_eq!(distro.family, Some(Family::Arch));
    }

    #[test]
    fn family_names_round_trip_through_the_report_field() {
        for family in [Family::Debian, Family::Fedora, Family::Arch] {
            assert_eq!(Family::from_name(family.as_str()), Some(family));
        }
        assert_eq!(Family::from_name("unsupported"), None);
    }

    #[test]
    fn comments_blank_lines_and_unknown_keys_are_ignored() {
        let distro = parse("# comment\n\nID=ubuntu\nVERSION_ID=24.04\n").expect("valid os-release");
        assert_eq!(distro.family, Some(Family::Debian));
    }

    #[test]
    fn unsupported_distro_gets_a_non_empty_manual_list() {
        let distro =
            parse("NAME=\"openSUSE\"\nID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n")
                .expect("valid os-release");
        assert_eq!(distro.family, None);
        assert!(!distro.manual_dependencies.is_empty());
    }

    #[test]
    fn values_may_be_quoted() {
        let distro = parse("ID='fedora'\n").expect("valid os-release");
        assert_eq!(distro.family, Some(Family::Fedora));
    }

    #[test]
    fn a_missing_id_is_an_error_not_an_unsupported_distro() {
        assert!(parse("NAME=\"Not an os-release\"\n").is_err());
        assert!(parse("ID=\n").is_err());
    }
}
