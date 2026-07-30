use crate::model::SrcInfo;
use anyhow::{Context, Result};
use camino::Utf8Path;
use std::fs;

pub fn parse_file(path: &Utf8Path) -> Result<SrcInfo> {
    let text = fs::read_to_string(path).with_context(|| format!("failed to read {path}"))?;
    Ok(parse(&text))
}

pub fn parse(text: &str) -> SrcInfo {
    let mut info = SrcInfo {
        pkgbase: None,
        pkgname: Vec::new(),
        pkgver: None,
        pkgrel: None,
        depends: Vec::new(),
        makedepends: Vec::new(),
        checkdepends: Vec::new(),
        optdepends: Vec::new(),
    };

    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().to_owned();
        match key {
            "pkgbase" => info.pkgbase = Some(value),
            "pkgname" => info.pkgname.push(value),
            "pkgver" => info.pkgver = Some(value),
            "pkgrel" => info.pkgrel = Some(value),
            key if key == "depends" || key.starts_with("depends_") => info.depends.push(value),
            key if key == "makedepends" || key.starts_with("makedepends_") => {
                info.makedepends.push(value)
            }
            key if key == "checkdepends" || key.starts_with("checkdepends_") => {
                info.checkdepends.push(value)
            }
            key if key == "optdepends" || key.starts_with("optdepends_") => {
                info.optdepends.push(value)
            }
            _ => {}
        }
    }

    info
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_repeated_fields() {
        let info = parse(
            r#"
pkgbase = example
	pkgver = 1.2.3
	pkgrel = 1
	pkgname = example
	depends = glibc
	depends = foo>=1
	depends_x86_64 = arch-runtime
	makedepends = git
	makedepends_x86_64 = arch-make
	checkdepends_x86_64 = arch-check
	optdepends = bar: optional thing
	optdepends_x86_64 = arch-opt: optional arch thing
"#,
        );

        assert_eq!(info.pkgbase.as_deref(), Some("example"));
        assert_eq!(info.pkgver.as_deref(), Some("1.2.3"));
        assert_eq!(info.depends, ["glibc", "foo>=1", "arch-runtime"]);
        assert_eq!(info.makedepends, ["git", "arch-make"]);
        assert_eq!(info.checkdepends, ["arch-check"]);
        assert_eq!(
            info.optdepends,
            ["bar: optional thing", "arch-opt: optional arch thing"]
        );
    }
}
