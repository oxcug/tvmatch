use std::{
    fs::{File, Metadata},
    io,
    path::Path,
};
#[cfg(windows)]
pub(super) fn identity(file: &File, m: &Metadata) -> io::Result<((u64, u64), (u64, u64))> {
    use std::os::windows::{fs::MetadataExt, io::AsRawHandle};
    // BY_HANDLE_FILE_INFORMATION is thirteen DWORDs, including three FILETIMEs.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut u32) -> i32;
    }
    let mut info = [0u32; 13];
    // SAFETY: valid open handle and correctly sized/aligned writable structure.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        (info[7] as u64, ((info[11] as u64) << 32) | info[12] as u64),
        (m.creation_time(), m.file_attributes() as u64),
    ))
}
#[cfg(unix)]
pub(super) fn identity(_: &File, m: &Metadata) -> io::Result<((u64, u64), (u64, u64))> {
    use std::os::unix::fs::MetadataExt;
    Ok((
        (m.dev(), m.ino()),
        (m.ctime() as u64, m.ctime_nsec() as u64),
    ))
}
#[cfg(not(any(windows, unix)))]
pub(super) fn identity(_: &File, _: &Metadata) -> io::Result<((u64, u64), (u64, u64))> {
    Err(io::Error::other("source identity unsupported on this OS"))
}
#[cfg(windows)]
pub(super) fn no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let wide = |p: &Path| -> io::Result<Vec<u16>> {
        let mut v: Vec<_> = p.as_os_str().encode_wide().collect();
        if v.contains(&0) {
            return Err(io::Error::other("NUL in rename path"));
        }
        v.push(0);
        Ok(v)
    };
    let (from, to) = (wide(from)?, wide(to)?);
    // SAFETY: NUL-terminated native strings; flags=0 forbids replacement and cross-volume copying.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::{
        ffi::{CString, c_char, c_int},
        os::unix::ffi::OsStrExt,
    };
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    let result = {
        unsafe extern "C" {
            fn renameat2(
                oldfd: c_int,
                old: *const c_char,
                newfd: c_int,
                new: *const c_char,
                flags: u32,
            ) -> c_int;
        }
        // SAFETY: valid C strings; AT_FDCWD=-100 and RENAME_NOREPLACE=1. No fallback.
        unsafe { renameat2(-100, from.as_ptr(), -100, to.as_ptr(), 1) }
    };
    #[cfg(target_os = "macos")]
    let result = {
        unsafe extern "C" {
            fn renamex_np(old: *const c_char, new: *const c_char, flags: u32) -> c_int;
        }
        // SAFETY: valid C strings; RENAME_EXCL=4 forbids replacing any destination.
        unsafe { renamex_np(from.as_ptr(), to.as_ptr(), 4) }
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "atomic no-replace rename refused (OS/filesystem support required): {}",
            io::Error::last_os_error()
        )))
    }
}
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(super) fn no_replace(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "atomic no-replace rename unsupported on this OS",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_no_replace_never_clobbers() {
        let root = std::env::temp_dir().join(format!("tvmatch-native-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let (a, b) = (root.join("source"), root.join("target"));
        std::fs::write(&a, b"source bytes").unwrap();
        std::fs::write(&b, b"target bytes").unwrap();
        assert!(no_replace(&a, &b).is_err());
        assert_eq!(std::fs::read(&a).unwrap(), b"source bytes");
        assert_eq!(std::fs::read(&b).unwrap(), b"target bytes");
        std::fs::remove_file(&b).unwrap();
        no_replace(&a, &b).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"source bytes");
        let f = File::open(&b).unwrap();
        assert!(identity(&f, &f.metadata().unwrap()).is_ok());
        drop(f);
        std::fs::remove_dir_all(root).unwrap();
    }
}
