use super::python_support;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Support(#[from] python_support::Error),
    #[error("malformed PURL: {input}")]
    Purl { input: String },
    #[error("required primary module was not found: {suffix}")]
    MissingSource { suffix: String },
    #[error("primary module {path} has the wrong {class:?} layout")]
    Mislocated { class: LayoutClass, path: PathBuf },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutClass {
    FlatSingleModule,
    FlatPackageDir,
    SrcLayout,
    LibPackageDir,
}
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Purl {
    pub ecosystem: String,
    pub name: String,
    pub version: String,
}
impl Purl {
    pub fn parse(input: &str) -> Result<Self, Error> {
        let mut parts = input.split('@');
        let Some(left) = parts.next() else {
            return Err(Error::Purl {
                input: input.into(),
            });
        };
        let Some(version) = parts.next() else {
            return Err(Error::Purl {
                input: input.into(),
            });
        };
        if parts.next().is_some() || version.is_empty() {
            return Err(Error::Purl {
                input: input.into(),
            });
        }
        let mut name = left.split(':');
        let Some(ecosystem) = name.next() else {
            return Err(Error::Purl {
                input: input.into(),
            });
        };
        let Some(name) = name.next() else {
            return Err(Error::Purl {
                input: input.into(),
            });
        };
        if ecosystem != "pypi" || name.is_empty() {
            return Err(Error::Purl {
                input: input.into(),
            });
        }
        Ok(Self {
            ecosystem: ecosystem.into(),
            name: name.into(),
            version: version.into(),
        })
    }
}
fn transport() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent()
}
pub fn locate(purl: &Purl) -> Result<(String, [u8; 32], String), Error> {
    let url = format!("https://pypi.org/pypi/{}/{}/json", purl.name, purl.version);
    let response = transport()
        .get(&url)
        .call()
        .map_err(|source| Error::Support(python_support::Error::Network { source }))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Error::Support(python_support::Error::Status { status }));
    }
    let mut text = String::new();
    response
        .into_body()
        .into_reader()
        .take(4 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|source| {
            Error::Support(python_support::Error::Read {
                observed: text.len(),
                source,
            })
        })?;
    let marker = "https://files.pythonhosted.org/packages/";
    let needle = format!("{}-{}", purl.name, purl.version).to_ascii_lowercase();
    let mut sdist = None;
    let mut digest = None;
    let mut wheel = None;
    for quoted in text.split('"') {
        let lowered = quoted.to_ascii_lowercase();
        if quoted.starts_with(marker) && quoted.ends_with(".tar.gz") && lowered.contains(&needle) {
            sdist = Some(quoted.to_owned());
        }
        if quoted.ends_with(".whl") && lowered.contains(&needle) {
            wheel = Some(quoted.to_owned());
        }
    }
    if let Some(url) = &sdist {
        if let Some(at) = text.find(url.as_str()) {
            if let Some(marker) = text[..at].rfind("\"digests\"") {
                if at - marker <= 2048 {
                    let section = &text[marker..at];
                    if let Some(at) = section.find("\"sha256\"") {
                        let after = &section[at + 8..];
                        let start = after.find('"').map_or(0, |n| n + 1);
                        let hex = &after[start..];
                        if hex.starts_with(|c: char| c.is_ascii_hexdigit())
                            && hex.get(64..65) == Some("\"")
                        {
                            digest = Some(decode_hex64(&hex[..64])?);
                        }
                    }
                }
            }
        }
    }
    match (sdist, digest, wheel) {
        (Some(url), Some(digest), Some(wheel)) => Ok((url, digest, wheel)),
        _ => Err(Error::MissingSource {
            suffix: "PyPI sdist metadata".into(),
        }),
    }
}
fn decode_hex64(text: &str) -> Result<[u8; 32], Error> {
    fn digit(byte: u8) -> Result<u8, Error> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(Error::Support(python_support::Error::Path {
                path: "sha256 hex".into(),
            })),
        }
    }
    let mut out = [0_u8; 32];
    for (i, pair) in text.as_bytes().chunks(2).enumerate() {
        out[i] = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Ok(out)
}
pub fn find_primary(root: &Path, class: LayoutClass, suffix: &[&str]) -> Result<PathBuf, Error> {
    fn walk(path: &Path, suffix: &[&str], found: &mut Vec<PathBuf>) -> io::Result<()> {
        for entry in std::fs::read_dir(path)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, suffix, found)?;
            } else if path
                .components()
                .rev()
                .zip(suffix.iter().rev())
                .all(|(part, wanted)| part.as_os_str() == *wanted)
            {
                found.push(path);
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(root, suffix, &mut found)
        .map_err(|source| Error::Support(python_support::Error::Io { source }))?;
    found.sort();
    for path in found {
        let relative = path.strip_prefix(root).map_err(|_| {
            Error::Support(python_support::Error::Path {
                path: path.display().to_string(),
            })
        })?;
        let components: Vec<_> = relative.components().collect();
        let marker = components
            .get(components.len().saturating_sub(suffix.len() + 1))
            .map(|c| c.as_os_str());
        let valid = match class {
            LayoutClass::FlatSingleModule => suffix.len() == 1 && components.len() == 2,
            LayoutClass::FlatPackageDir => {
                components.len() == suffix.len() + 1
                    && marker.is_some_and(|x| x != "src" && x != "lib")
            }
            LayoutClass::SrcLayout => {
                marker.is_some_and(|x| x == "src") || components.len() == suffix.len() + 1
            }
            LayoutClass::LibPackageDir => marker.is_some_and(|x| x == "lib"),
        };
        if valid {
            return Ok(path);
        }
        return Err(Error::Mislocated { class, path });
    }
    Err(Error::MissingSource {
        suffix: suffix.join("/"),
    })
}

#[cfg(test)]
mod tests {
    use super::{Error, LayoutClass, find_primary};
    fn archive(path: &str) -> Result<Vec<u8>, super::super::python_support::Error> {
        use super::super::python_support::tests as f;
        let mut tar = f::entry(&f::make_name(path), b"primary");
        tar.extend_from_slice(&f::entry(&f::make_name("decoy.py"), b"decoy"));
        tar.extend_from_slice(&[0_u8; 1024]);
        f::gz(&tar)
    }
    fn positive(class: LayoutClass, path: &str, suffix: &[&str]) -> Result<(), Error> {
        let root = super::super::python_support::fresh_dir("layout")?;
        super::super::python_support::unpack(&archive(path)?, &root)?;
        if find_primary(&root, class, suffix).is_err() {
            return Err(Error::MissingSource {
                suffix: suffix.join("/"),
            });
        }
        std::fs::remove_dir_all(root)
            .map_err(|source| Error::Support(super::super::python_support::Error::Io { source }))?;
        Ok(())
    }
    fn negative(class: LayoutClass, suffix: &[&str]) -> Result<(), Error> {
        let root = super::super::python_support::fresh_dir("layout")?;
        super::super::python_support::unpack(&archive("package/decoy.py")?, &root)?;
        if !matches!(find_primary(&root, class, suffix), Err(Error::MissingSource { suffix: ref actual }) if *actual == suffix.join("/"))
        {
            return Err(Error::MissingSource {
                suffix: suffix.join("/"),
            });
        }
        std::fs::remove_dir_all(root)
            .map_err(|source| Error::Support(super::super::python_support::Error::Io { source }))?;
        Ok(())
    }
    #[test]
    fn flat_positive() -> Result<(), Error> {
        positive(LayoutClass::FlatSingleModule, "package/six.py", &["six.py"])
    }
    #[test]
    fn flat_absent_rejects_decoy() -> Result<(), Error> {
        negative(LayoutClass::FlatSingleModule, &["six.py"])
    }
    #[test]
    fn src_positive() -> Result<(), Error> {
        positive(
            LayoutClass::SrcLayout,
            "package/src/idna/core.py",
            &["idna", "core.py"],
        )
    }
    #[test]
    fn src_absent_rejects_decoy() -> Result<(), Error> {
        negative(LayoutClass::SrcLayout, &["idna", "core.py"])
    }
    #[test]
    fn lib_positive() -> Result<(), Error> {
        positive(
            LayoutClass::LibPackageDir,
            "package/lib/yaml/__init__.py",
            &["yaml", "__init__.py"],
        )
    }
    #[test]
    fn lib_absent_rejects_decoy() -> Result<(), Error> {
        negative(LayoutClass::LibPackageDir, &["yaml", "__init__.py"])
    }
}
