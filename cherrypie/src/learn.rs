// Rule learning. A normal window whose class no rule matches gets a rule
// appended to the config with live default actions, so it is placed at
// once and the user edits the numbers to taste. The appended text is the
// single source: It is parsed and compiled by the same path as the file,
// so what is applied is exactly what was written.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::config::Config;
use crate::rules::{self, CompiledRule};

const FILE_HEADER: &str = "\
# cherrypie rules. A window class with no rule gets one appended here with
# default actions the first time it maps; edit the values to taste.
#   matchers (regex): class, title, role, process; type (exact)
#   actions: position, size, monitor, workspace, maximize, fullscreen, pin,
#            minimize, shade, above, below, decorate, focus, opacity
";

const DEFAULT_ACTIONS: &str = "\
position = \"center\"
size = [\"60%\", \"70%\"]
focus = true
";

// The rule text appended for `class`: An anchored, escaped class matcher
// plus the default actions.
pub fn rule_text(class: &str, date: &str) -> String {
    format!(
        "\n# Learned {} from a new window; edit to taste.\n[[rule]]\nclass = {}\n{}",
        date,
        toml_string(&format!("^{}$", regex::escape(class))),
        DEFAULT_ACTIONS
    )
}

// A literal string keeps the regex readable; a basic string only when the
// value holds what a literal cannot.
fn toml_string(s: &str) -> String {
    if s.contains('\'') || s.chars().any(|c| c.is_control()) {
        toml::Value::String(s.to_string()).to_string()
    } else {
        format!("'{}'", s)
    }
}

pub fn compile(text: &str) -> Result<CompiledRule, String> {
    let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut compiled = rules::compile(&cfg)?;
    compiled.pop().ok_or_else(|| "learned text holds no rule".to_string())
}

// Append `text` to the config, creating it (with the header) when absent.
// A file that does not end in a newline gets one first, so the appended
// table never joins the user's last line.
pub fn append(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().read(true).append(true).create(true).open(path)?;
    let len = f.metadata()?.len();
    let mut out = String::with_capacity(FILE_HEADER.len() + text.len() + 1);
    if len == 0 {
        out.push_str(FILE_HEADER);
    } else {
        let mut last = [0u8; 1];
        f.seek(SeekFrom::End(-1))?;
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            out.push('\n');
        }
    }
    out.push_str(text);
    f.write_all(out.as_bytes())
}

// Local date as YYYY-MM-DD for the learned-rule comment.
pub fn today() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
    }
}
