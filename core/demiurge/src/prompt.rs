// The run prompt: What is typed into the bar after the run_prompt bind, its
// cursor and the executable on PATH it would complete to. Both stacks feed
// it keysyms and draw it; Enter hands back the command to run.

use std::os::unix::fs::PermissionsExt;

#[derive(Default)]
pub struct Prompt {
    pub input: String,
    pub cursor: usize,
    pub completion: Option<String>,
}

pub enum Key {
    /// Enter on a command: The prompt closes and the command runs.
    Run(String),
    /// Escape, or Enter on nothing: The prompt closes.
    Closed,
    /// Anything else: The prompt stays open.
    Open,
}

impl Prompt {
    pub fn key(&mut self, keysym: u32, executables: &[String]) -> Key {
        match keysym {
            // Return
            0xff0d => {
                if self.input.is_empty() {
                    return Key::Closed;
                }
                return Key::Run(self.input.clone());
            }
            // Escape
            0xff1b => return Key::Closed,
            // Tab: Accept the completion
            0xff09 => {
                if let Some(ref comp) = self.completion {
                    self.input = comp.clone();
                    self.cursor = self.input.len();
                    self.completion = find_completion(executables, &self.input);
                }
            }
            // BackSpace
            0xff08 => {
                if self.cursor > 0 {
                    self.input.remove(self.cursor - 1);
                    self.cursor -= 1;
                    self.completion = find_completion(executables, &self.input);
                }
                if self.input.is_empty() {
                    self.completion = None;
                }
            }
            // Printable ASCII
            sym if (0x0020..=0x007e).contains(&sym) => {
                self.input.insert(self.cursor, sym as u8 as char);
                self.cursor += 1;
                self.completion = find_completion(executables, &self.input);
            }
            _ => {}
        }
        Key::Open
    }
}

pub fn scan_path() -> Vec<String> {
    let path = match std::env::var("PATH") {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    let mut names: Vec<String> = Vec::new();

    for dir in path.split(':') {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            if meta.permissions().mode() & 0o111 == 0 {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }

    names.sort_unstable();
    names.dedup();
    names
}

pub fn find_completion(executables: &[String], input: &str) -> Option<String> {
    if input.is_empty() {
        return None;
    }
    executables
        .iter()
        .find(|name| name.starts_with(input))
        .cloned()
}
