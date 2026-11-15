//! Path arguments for the C++ sidecars (whisper-cli, parakeet-cli,
//! sherpa-onnx diarization).
//!
//! On Windows those programs read `argv` through the ANSI code page, so a
//! `ñ` or `ú` in the user's profile folder (`C:\Users\Núñez\AppData\…`,
//! where every app path lives) is mangled and the sidecar dies with
//! 0xC0000409. Rust itself handles Unicode paths fine, so the fix is
//! entirely in what we pass on the command line: [`prepare`] turns the
//! paths a sidecar needs into arguments it can read.
//!
//! Elsewhere (macOS/Linux, UTF-8 argv) it is the identity.

use std::path::{Component, Path, PathBuf};

/// Substring of the error [`prepare`] returns when a path cannot be made
/// safe for a sidecar. The job scheduler treats it as permanent: retrying
/// the identical paths three times can never work.
pub const NON_ASCII_PATH_MARKER: &str = "non-ASCII file path";

/// Paths ready to hand to a sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarPaths {
    /// One entry per input path, same order. Pass these as arguments.
    pub args: Vec<PathBuf>,
    /// When set, spawn the sidecar with this working directory — the
    /// `args` are then ASCII paths relative to it.
    pub current_dir: Option<PathBuf>,
}

/// Make `paths` (input files, models, not-yet-created output files) safe
/// to pass as sidecar arguments. See the crate docs.
///
/// Strategy, per path: ASCII → unchanged; else its 8.3 short form (for a
/// file that does not exist yet: short parent + the ASCII file name); if
/// any path still has no ASCII form (short names disabled), run the sidecar
/// from the paths' common ancestor with ASCII relative paths; if even that
/// is impossible, an actionable error containing [`NON_ASCII_PATH_MARKER`].
pub fn prepare(paths: &[&Path]) -> Result<SidecarPaths, String> {
    // macOS/Linux sidecars take UTF-8 argv: nothing to work around.
    #[cfg(not(windows))]
    {
        Ok(SidecarPaths {
            args: paths.iter().map(|p| p.to_path_buf()).collect(),
            current_dir: None,
        })
    }
    #[cfg(windows)]
    {
        resolve(paths, &platform_short_path)
    }
}

/// The "short names are disabled" strategy on its own — lets tests drive a
/// real sidecar through the relative-path fallback on a machine where 8.3
/// names work. Not for product code.
#[doc(hidden)]
pub fn prepare_relative_only(paths: &[&Path]) -> Result<SidecarPaths, String> {
    resolve(paths, &|_| None)
}

fn is_ascii_path(p: &Path) -> bool {
    p.as_os_str().to_str().is_some_and(str::is_ascii)
}

fn resolve(
    paths: &[&Path],
    short: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Result<SidecarPaths, String> {
    if paths.iter().all(|p| is_ascii_path(p)) {
        return Ok(SidecarPaths {
            args: paths.iter().map(|p| p.to_path_buf()).collect(),
            current_dir: None,
        });
    }

    let shortened: Option<Vec<PathBuf>> = paths.iter().map(|p| shorten_one(p, short)).collect();
    if let Some(args) = shortened {
        return Ok(SidecarPaths {
            args,
            current_dir: None,
        });
    }

    relative_to_common_ancestor(paths)
}

fn shorten_one(p: &Path, short: &dyn Fn(&Path) -> Option<PathBuf>) -> Option<PathBuf> {
    if is_ascii_path(p) {
        return Some(p.to_path_buf());
    }
    let ascii = |s: PathBuf| is_ascii_path(&s).then_some(s);
    if p.exists() {
        return short(p).and_then(ascii);
    }
    // An output file that does not exist yet has no short name of its own:
    // shorten the folder it will be created in and keep its (ASCII) name.
    let name = p.file_name()?;
    if !is_ascii_path(Path::new(name)) {
        return None;
    }
    let parent = p.parent()?;
    short(parent).and_then(ascii).map(|s| s.join(name))
}

fn relative_to_common_ancestor(paths: &[&Path]) -> Result<SidecarPaths, String> {
    let absolute: Vec<PathBuf> = paths
        .iter()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
        .collect();
    let mut ancestor: Vec<Component> = absolute[0]
        .parent()
        .unwrap_or(&absolute[0])
        .components()
        .collect();
    for p in &absolute[1..] {
        let comps: Vec<Component> = p.parent().unwrap_or(p).components().collect();
        let common = ancestor
            .iter()
            .zip(comps.iter())
            .take_while(|(a, b)| a == b)
            .count();
        ancestor.truncate(common);
    }
    let ancestor: PathBuf = ancestor.iter().collect();
    // No shared ancestor (different drives): nothing to be relative to.
    if ancestor.as_os_str().is_empty() {
        return Err(unsupported_path_error(paths));
    }

    let mut args = Vec::with_capacity(absolute.len());
    for p in &absolute {
        match p.strip_prefix(&ancestor) {
            Ok(rel) if !rel.as_os_str().is_empty() && is_ascii_path(rel) => {
                args.push(rel.to_path_buf())
            }
            _ => return Err(unsupported_path_error(paths)),
        }
    }
    Ok(SidecarPaths {
        args,
        current_dir: Some(ancestor),
    })
}

fn unsupported_path_error(paths: &[&Path]) -> String {
    let offending = paths
        .iter()
        .find(|p| !is_ascii_path(p))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    format!(
        "The audio engine cannot open a {NON_ASCII_PATH_MARKER} ({offending}): it contains \
         letters outside plain English and Windows has no short (8.3) name for the folder. \
         To fix it, give that folder a short name (an administrator can run \
         `fsutil file setshortname <folder> <SHORTNAME>`), or use a Windows account whose \
         profile folder name is plain English letters and digits."
    )
}

/// The 8.3 short form of an existing path (Windows).
#[cfg(windows)]
fn platform_short_path(p: &Path) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, cch: u32) -> u32;
    }

    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    // First call sizes the buffer (returns the length incl. NUL), the second
    // fills it; loop once more if the path changed length in between.
    let mut buf: Vec<u16> = vec![0; 512];
    for _ in 0..3 {
        // SAFETY: `wide` is NUL-terminated and `buf` is a valid writable
        // buffer of exactly `buf.len()` u16s.
        let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        if n == 0 {
            return None;
        }
        if (n as usize) < buf.len() {
            return Some(PathBuf::from(OsString::from_wide(&buf[..n as usize])));
        }
        buf.resize(n as usize + 1, 0);
    }
    None
}

#[cfg(not(windows))]
fn platform_short_path(p: &Path) -> Option<PathBuf> {
    // Never reached for a path the OS needs help with: other platforms pass
    // UTF-8 argv through, so `prepare` is the identity there.
    Some(p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NON_ASCII: &str = "Ünicode Tëst ñ";

    fn no_short(_: &Path) -> Option<PathBuf> {
        None
    }

    #[test]
    fn ascii_paths_pass_through_unchanged() {
        let a = Path::new("C:/Users/plain/AppData/model.bin");
        let b = Path::new("C:/Users/plain/AppData/out");
        let r = prepare(&[a, b]).unwrap();
        assert_eq!(r.args, vec![a.to_path_buf(), b.to_path_buf()]);
        assert_eq!(r.current_dir, None);
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_is_the_identity_even_for_non_ascii_paths() {
        let a = Path::new("/home/josé/.local/share/model.bin");
        let b = Path::new("/tmp/ñ/chunk.wav");
        let r = prepare(&[a, b]).unwrap();
        assert_eq!(r.args, vec![a.to_path_buf(), b.to_path_buf()]);
        assert_eq!(r.current_dir, None);
    }

    /// Cross-platform: the resolution logic with an injected shortener.
    #[test]
    fn shortener_is_used_for_existing_and_for_new_output_files() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join(NON_ASCII);
        std::fs::create_dir(&folder).unwrap();
        let existing = folder.join("in.wav");
        std::fs::write(&existing, b"x").unwrap();
        let output = folder.join("out-123");

        let folder_c = folder.clone();
        let short = move |p: &Path| -> Option<PathBuf> {
            if p == folder_c {
                Some(PathBuf::from("C:/SHORT~1"))
            } else if p.parent() == Some(folder_c.as_path()) {
                Some(PathBuf::from("C:/SHORT~1").join("IN~1.WAV"))
            } else {
                None
            }
        };
        let r = resolve(&[&existing, &output], &short).unwrap();
        assert_eq!(
            r.args,
            vec![
                PathBuf::from("C:/SHORT~1/IN~1.WAV"),
                PathBuf::from("C:/SHORT~1/out-123")
            ]
        );
        assert_eq!(r.current_dir, None);
    }

    #[test]
    fn falls_back_to_relative_paths_from_the_common_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("models").join(NON_ASCII).join("m.bin");
        let wav = dir.path().join("tmp").join("chunk.wav");
        let r = resolve(&[&model, &wav], &no_short);
        // `models/Ünicode…` has a non-ASCII component below the ancestor:
        assert!(r.unwrap_err().contains(NON_ASCII_PATH_MARKER));

        // Non-ASCII only ABOVE the ancestor → relative ASCII paths work.
        let root = dir.path().join(NON_ASCII);
        let model = root.join("models").join("m.bin");
        let wav = root.join("tmp").join("chunk.wav");
        let r = resolve(&[&model, &wav], &no_short).unwrap();
        assert_eq!(r.current_dir.as_deref(), Some(root.as_path()));
        assert_eq!(r.args[0], Path::new("models").join("m.bin"));
        assert_eq!(r.args[1], Path::new("tmp").join("chunk.wav"));
        assert!(r.args.iter().all(|a| is_ascii_path(a)));
    }

    #[test]
    fn unfixable_paths_give_an_actionable_marked_error() {
        let p = Path::new("C:/Users/Núñez/x/model.bin");
        let q = Path::new("D:/other/chunk.wav"); // different drive: no ancestor
        let e = resolve(&[p, q], &no_short).unwrap_err();
        assert!(e.contains(NON_ASCII_PATH_MARKER));
        assert!(e.contains("Núñez"));
        assert!(e.contains("setshortname"));
    }

    #[cfg(windows)]
    mod windows_only {
        use super::*;

        fn short_names_enabled(dir: &Path) -> bool {
            platform_short_path(dir).is_some_and(|s| is_ascii_path(&s))
        }

        #[test]
        fn non_ascii_dir_becomes_an_ascii_path_to_the_same_file() {
            let base = tempfile::tempdir().unwrap();
            let dir = base.path().join(NON_ASCII);
            std::fs::create_dir(&dir).unwrap();
            let file = dir.join("meeting recording.wav");
            std::fs::write(&file, b"hello").unwrap();
            if !short_names_enabled(&dir) {
                eprintln!("8.3 names are disabled here; skipping");
                return;
            }

            let r = prepare(&[&file]).unwrap();
            assert!(is_ascii_path(&r.args[0]), "{:?}", r.args[0]);
            assert_eq!(std::fs::read(&r.args[0]).unwrap(), b"hello");
            assert_eq!(r.current_dir, None);
        }

        #[test]
        fn output_file_that_does_not_exist_yet_gets_short_parent_and_ascii_name() {
            let base = tempfile::tempdir().unwrap();
            let dir = base.path().join(NON_ASCII);
            std::fs::create_dir(&dir).unwrap();
            if !short_names_enabled(&dir) {
                return;
            }
            let out = dir.join("flyonthewall-whisper-1234");

            let r = prepare(&[&out]).unwrap();
            assert!(is_ascii_path(&r.args[0]), "{:?}", r.args[0]);
            assert_eq!(r.args[0].file_name().unwrap(), "flyonthewall-whisper-1234");
            // creating the file through the short path lands in the real folder
            std::fs::write(&r.args[0], b"x").unwrap();
            assert!(out.exists());
        }
    }
}
