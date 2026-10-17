// GNOSIS filesystem layer: read a directory into a sorted Entry list.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    // Symlink target resolution: is_dir reflects the target. Kept for
    // a future "→ link" glyph.
    pub is_symlink: bool,
}

// Read `dir` into a sorted entry list: directories first, then files,
// each group case-insensitively alphabetical. Hidden entries (dotfiles)
// are included only when `show_hidden`. Unreadable entries are skipped;
// an unreadable directory yields an empty list (the caller shows the
// path is empty / denied rather than crashing).
pub fn read_dir(dir: &Path, show_hidden: bool) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return entries,
    };
    for item in rd.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let path = item.path();
        let is_symlink = item
            .file_type()
            .map(|t| t.is_symlink())
            .unwrap_or(false);
        // metadata() follows symlinks, so is_dir reflects the target.
        let is_dir = std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
        entries.push(Entry {
            name,
            path,
            is_dir,
            is_symlink,
        });
    }
    sort(&mut entries);
    entries
}

// Peek the first `n` entries inside `dir` for the W10-style folder
// preview: returns each peeked entry's is_dir flag (dirs first, like
// the full sort, so the preview cards lead with subfolders). Cheap --
// stops after collecting a small sample, no full read of huge dirs.
// Hidden entries are skipped. Returns the total visible count too so
// the preview can show "+N more".
pub fn peek(dir: &Path, n: usize) -> (Vec<bool>, usize) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return (Vec::new(), 0),
    };
    let mut dirs: Vec<bool> = Vec::new();
    let mut files: Vec<bool> = Vec::new();
    let mut total = 0usize;
    for item in rd.flatten() {
        let name = item.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        total += 1;
        let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
        // Resolve symlinks cheaply only while we still need samples.
        if dirs.len() + files.len() < n * 2 {
            let resolved_dir = if is_dir {
                true
            } else {
                std::fs::metadata(item.path()).map(|m| m.is_dir()).unwrap_or(false)
            };
            if resolved_dir {
                dirs.push(true);
            } else {
                files.push(false);
            }
        }
    }
    let mut sample: Vec<bool> = Vec::with_capacity(n);
    sample.extend(dirs.into_iter());
    sample.extend(files.into_iter());
    sample.truncate(n);
    (sample, total)
}

// Directories first, then files; each case-insensitively alphabetical.
pub fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

// Bookmarks for the picker: Home, Root, then the user's GTK bookmarks
// (~/.config/gtk-3.0/bookmarks -- one `file:///path [optional label]`
// per line), so GNOSIS interoperates with every other file manager.
pub fn bookmarks() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        out.push(("Home".into(), PathBuf::from(home)));
    }
    out.push(("Root".into(), PathBuf::from("/")));
    if let Some(home) = std::env::var_os("HOME") {
        let path = PathBuf::from(home)
            .join(".config")
            .join("gtk-3.0")
            .join("bookmarks");
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("file://") {
                    // Split off an optional display label after a space.
                    let (uri, label) = match rest.split_once(' ') {
                        Some((u, l)) => (u, Some(l.to_string())),
                        None => (rest, None),
                    };
                    let p = PathBuf::from(url_decode(uri));
                    let name = label.unwrap_or_else(|| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| uri.to_string())
                    });
                    out.push((name, p));
                }
            }
        }
    }
    out
}

// Move `path` to the XDG trash ($XDG_DATA_HOME/Trash, default
// ~/.local/share/Trash) per the freedesktop trash spec: the file goes
// to files/<name>, and info/<name>.trashinfo records the original
// path + deletion date so a trash UI can restore it.
pub fn trash(path: &Path) -> Result<(), String> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or("no HOME / XDG_DATA_HOME")?;
    let trash = data_home.join("Trash");
    let files = trash.join("files");
    let info = trash.join("info");
    std::fs::create_dir_all(&files).map_err(|e| format!("mkdir trash/files: {}", e))?;
    std::fs::create_dir_all(&info).map_err(|e| format!("mkdir trash/info: {}", e))?;

    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("path has no file name")?;
    // Resolve a non-colliding name in the trash.
    let mut name = base.clone();
    let mut i = 1;
    while files.join(&name).exists() || info.join(format!("{}.trashinfo", name)).exists() {
        name = format!("{}.{}", base, i);
        i += 1;
    }

    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let info_body = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        url_encode(&abs.to_string_lossy()),
        now_iso8601(),
    );
    std::fs::write(info.join(format!("{}.trashinfo", name)), info_body)
        .map_err(|e| format!("write trashinfo: {}", e))?;
    std::fs::rename(path, files.join(&name)).map_err(|e| format!("move to trash: {}", e))?;
    Ok(())
}

// Copy `src` (file or directory) into `dst_dir`, keeping its base
// name. Recursive for directories.
pub fn copy_into(src: &Path, dst_dir: &Path) -> Result<(), String> {
    let base = src.file_name().ok_or("src has no file name")?;
    let dst = dst_dir.join(base);
    copy_recursive(src, &dst)
}

fn copy_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(src).map_err(|e| format!("stat {:?}: {}", src, e))?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {:?}: {}", dst, e))?;
        for item in std::fs::read_dir(src).map_err(|e| format!("read_dir {:?}: {}", src, e))? {
            let item = item.map_err(|e| format!("dirent: {}", e))?;
            copy_recursive(&item.path(), &dst.join(item.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ()).map_err(|e| format!("copy: {}", e))
    }
}

// Minimal percent-encoding for trashinfo Path= (RFC 3986 unreserved +
// '/' kept literal, everything else encoded).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn now_iso8601() -> String {
    unsafe {
        let mut buf = [0i8; 32];
        let mut tm: libc::tm = std::mem::zeroed();
        let now = libc::time(std::ptr::null_mut());
        libc::localtime_r(&now, &mut tm);
        let fmt = std::ffi::CString::new("%Y-%m-%dT%H:%M:%S").unwrap();
        let n = libc::strftime(buf.as_mut_ptr(), buf.len(), fmt.as_ptr(), &tm);
        if n == 0 {
            return String::new();
        }
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}
