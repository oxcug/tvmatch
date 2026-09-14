//! Native paths at CLI/environment boundaries; no shell evaluation or Unix slash rewriting.
use std::{
    ffi::OsStr,
    io,
    path::{Component, Path, PathBuf},
};

pub fn input(path: &Path) -> io::Result<PathBuf> {
    resolve(
        path,
        std::env::var_os("HOME").as_deref().map(Path::new),
        &std::env::current_dir()?,
        cfg!(windows),
    )
}
fn resolve(path: &Path, home: Option<&Path>, cwd: &Path, windows: bool) -> io::Result<PathBuf> {
    let converted = windows_drive(path, windows);
    let mut parts = converted.components();
    let expanded = if parts.next() == Some(Component::Normal(OsStr::new("~"))) {
        home.ok_or_else(|| io::Error::other("HOME required for leading ~"))?
            .join(parts.as_path())
    } else {
        converted
    };
    // HOME may itself use an MSYS drive path; normalize after substitution too.
    let expanded = windows_drive(&expanded, windows);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    // Resolve '.' lexically, but refuse '..' for cache/input consistency rather than following links.
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => return Err(io::Error::other("path parent traversal refused")),
            Component::CurDir => (),
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}
fn windows_drive(path: &Path, windows: bool) -> PathBuf {
    if windows && let Some(s) = path.to_str() {
        let b = s.as_bytes();
        if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
            return PathBuf::from(format!("{}:{}", b[1] as char, &s[2..]));
        }
    }
    path.to_owned()
}
pub fn reference_cache() -> io::Result<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")));
    let base = base
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| io::Error::other("no user cache directory configured"))?;
    Ok(input(&base)?.join("tvmatch/references/opensubtitles-v1"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drive_conversion_only_on_windows_boundary() {
        assert_eq!(
            windows_drive(Path::new("/c/Users/a space/ü"), true),
            PathBuf::from("c:/Users/a space/ü")
        );
        assert_eq!(
            windows_drive(Path::new("C:/Users/a"), true),
            PathBuf::from("C:/Users/a")
        );
        assert_eq!(
            windows_drive(Path::new("/home/a"), true),
            PathBuf::from("/home/a")
        );
        assert_eq!(
            windows_drive(Path::new("/c/a"), false),
            PathBuf::from("/c/a")
        );
    }
    #[test]
    fn relative_home_spaces_and_traversal() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            resolve(Path::new("./a ü"), None, &cwd, false).unwrap(),
            cwd.join("a ü")
        );
        assert_eq!(
            resolve(Path::new("~/a space"), Some(&cwd), &cwd, false).unwrap(),
            cwd.join("a space")
        );
        assert!(resolve(Path::new("../escape"), None, &cwd, false).is_err());
        assert_eq!(
            resolve(Path::new("$NOT_EXPANDED"), None, &cwd, false).unwrap(),
            cwd.join("$NOT_EXPANDED")
        );
    }
    #[cfg(windows)]
    #[test]
    fn tilde_msys_home_ignores_other_cwd_drive() {
        assert_eq!(
            resolve(
                Path::new("~/Video"),
                Some(Path::new("/c/Users/example")),
                Path::new("D:/work"),
                true,
            )
            .unwrap(),
            PathBuf::from("c:/Users/example/Video")
        );
    }
    #[cfg(unix)]
    #[test]
    fn unix_non_utf8_and_backslashes_survive() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(OsStr::from_bytes(b"a\\b\xff"));
        assert_eq!(
            resolve(path, None, Path::new("/home/user"), false).unwrap(),
            Path::new("/home/user").join(path)
        );
    }
}
