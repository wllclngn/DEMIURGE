// lxappearance replacement.
//
// This module owns the cursor theme and system default font that
// lxappearance used to manage. DEMIURGE is the single source of truth;
// the files lxappearance would write (~/.gtkrc-2.0,
// ~/.config/gtk-3.0/settings.ini, ~/.icons/default/index.theme) are
// regenerated from [cursor] and [font] in config.toml via `demiurge
// --setup`. With this in place, lxappearance can be uninstalled.
//
// Two entry points:
//   - apply_runtime: called at WM init; exports XCURSOR_THEME and
//     XCURSOR_SIZE into the process environment so every child inherits
//     the cursor theme. libXcursor checks these env vars before falling
//     back to RESOURCE_MANAGER.
//   - write_setup_files: called by `demiurge --setup`; writes the
//     lxappearance-equivalent files from config. Idempotent.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::Config;

pub fn apply_runtime(config: &Config) {
    // SAFETY: single-threaded process at this point (we haven't spawned
    // any children or started the event loop yet).
    unsafe {
        std::env::set_var("XCURSOR_THEME", &config.cursor.theme);
        std::env::set_var("XCURSOR_SIZE", config.cursor.size.to_string());
    }
}

pub fn write_setup_files(config: &Config) -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let home = PathBuf::from(home);

    write_icons_default(&home, &config.cursor.theme)?;
    write_gtk3_settings(&home, config)?;
    write_gtk2_settings(&home, config)?;

    Ok(())
}

// ~/.icons/default/index.theme: primary location libXcursor reads to
// determine the default cursor theme. Apps that don't check env vars
// or RESOURCE_MANAGER still honor this.
fn write_icons_default(home: &Path, cursor_theme: &str) -> Result<(), String> {
    let dir = home.join(".icons").join("default");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;

    let contents = format!(
        "[Icon Theme]\nName=Default\nComment=Default Cursor Theme\nInherits={}\n",
        cursor_theme
    );
    let path = dir.join("index.theme");
    atomic_write(&path, contents.as_bytes())?;
    eprintln!("[demiurge setup] wrote {}", path.display());
    Ok(())
}

// ~/.config/gtk-3.0/settings.ini: GTK3 app font and cursor theme. Merge
// onto existing file (preserves theme-name and other keys the user set).
fn write_gtk3_settings(home: &Path, config: &Config) -> Result<(), String> {
    let dir = home.join(".config").join("gtk-3.0");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;
    let path = dir.join("settings.ini");

    let updates = [
        ("gtk-font-name", config.font.default.clone()),
        ("gtk-cursor-theme-name", config.cursor.theme.clone()),
        ("gtk-cursor-theme-size", config.cursor.size.to_string()),
    ];
    merge_gtk_settings(&path, &updates)?;
    eprintln!("[demiurge setup] wrote {}", path.display());
    Ok(())
}

// ~/.gtkrc-2.0: GTK2 app font and cursor theme. Mostly legacy but still
// read by a few apps. Cheap to emit.
fn write_gtk2_settings(home: &Path, config: &Config) -> Result<(), String> {
    let path = home.join(".gtkrc-2.0");
    let contents = format!(
        "gtk-font-name=\"{}\"\ngtk-cursor-theme-name=\"{}\"\ngtk-cursor-theme-size={}\n",
        config.font.default, config.cursor.theme, config.cursor.size
    );
    atomic_write(&path, contents.as_bytes())?;
    eprintln!("[demiurge setup] wrote {}", path.display());
    Ok(())
}

// Merge key=value updates into an INI-style file. Preserves the [Settings]
// header and any keys not being updated. Creates the file with a minimal
// template if missing.
fn merge_gtk_settings(path: &Path, updates: &[(&str, String)]) -> Result<(), String> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = if existing.is_empty() {
        vec!["[Settings]".into()]
    } else {
        existing.lines().map(String::from).collect()
    };

    if !lines.iter().any(|l| l.trim() == "[Settings]") {
        lines.insert(0, "[Settings]".into());
    }

    for (key, value) in updates {
        let new_line = format!("{}={}", key, value);
        let mut replaced = false;
        for line in lines.iter_mut() {
            let trimmed = line.trim_start();
            if let Some(existing_key) = trimmed.split('=').next() {
                if existing_key.trim() == *key {
                    *line = new_line.clone();
                    replaced = true;
                    break;
                }
            }
        }
        if !replaced {
            lines.push(new_line);
        }
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    atomic_write(path, out.as_bytes())
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)
            .map_err(|e| format!("create {}: {}", tmp.display(), e))?;
        f.write_all(data)
            .map_err(|e| format!("write {}: {}", tmp.display(), e))?;
        f.sync_all()
            .map_err(|e| format!("fsync {}: {}", tmp.display(), e))?;
    }
    fs::rename(&tmp, path)
        .map_err(|e| format!("rename {} -> {}: {}", tmp.display(), path.display(), e))?;
    Ok(())
}
