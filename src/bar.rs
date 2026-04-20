use std::ffi::CString;
use std::os::unix::fs::PermissionsExt;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::atoms::Atoms;
use crate::config::Config;
use crate::layout::Layout;
use crate::monitor::Monitor;

struct BarPanel {
    window: Window,
    gc: Gcontext,
    width: u32,
}

pub struct Bar {
    panels: Vec<BarPanel>,
    pub height: u32,
    depth: u8,
    font_desc: pango::FontDescription,
    font_options: cairo::FontOptions,
    colors: BarColors,
    clock_format: String,
    tag_names: Vec<String>,
    pub path_executables: Vec<String>,
    pub prompt: Option<PromptState>,
    pub tag_extents: Vec<(i32, i32)>,
}

struct BarColors {
    bg: (f64, f64, f64),
    fg: (f64, f64, f64),
    tag_focused_bg: (f64, f64, f64),
    tag_focused_fg: (f64, f64, f64),
    tag_occupied_fg: (f64, f64, f64),
    tag_empty_fg: (f64, f64, f64),
}

pub struct PromptState {
    pub input: String,
    pub cursor: usize,
    pub completion: Option<String>,
}

impl Bar {
    pub fn create(
        conn: &RustConnection,
        root: Window,
        atoms: &Atoms,
        config: &Config,
        monitors: &[Monitor],
        tag_names: &[String],
    ) -> Result<Self, String> {
        let screen = &conn.setup().roots[0];
        let depth = screen.root_depth;
        let height = config.bar.height;
        let bg_pixel = crate::torrentius::hex_to_pixel(&config.bar.bg);

        let mut panels = Vec::new();

        for (i, mon) in monitors.iter().enumerate() {
            let window = conn.generate_id().map_err(|e| format!("bar window id: {}", e))?;
            let gc = conn.generate_id().map_err(|e| format!("bar gc id: {}", e))?;

            conn.create_window(
                depth,
                window,
                root,
                mon.x as i16,
                mon.y as i16,
                mon.width as u16,
                height as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .background_pixel(bg_pixel)
                    .event_mask(EventMask::EXPOSURE | EventMask::BUTTON_PRESS),
            )
            .map_err(|e| format!("create bar window: {}", e))?;

            let _ = conn.change_property32(
                PropMode::REPLACE,
                window,
                atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                &[atoms._NET_WM_WINDOW_TYPE_DOCK],
            );

            let strut: [u32; 12] = [
                0,
                0,
                height,
                0,
                0,
                0,
                0,
                0,
                mon.x as u32,
                mon.x as u32 + mon.width - 1,
                0,
                0,
            ];
            let _ = conn.change_property32(
                PropMode::REPLACE,
                window,
                atoms._NET_WM_STRUT_PARTIAL,
                AtomEnum::CARDINAL,
                &strut,
            );

            conn.create_gc(gc, window, &CreateGCAux::new())
                .map_err(|e| format!("create bar gc: {}", e))?;

            let _ = conn.map_window(window);
            let _ = conn.configure_window(
                window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );

            panels.push(BarPanel {
                window,
                gc,
                width: mon.width,
            });

            eprintln!(
                "[demiurge] bar {}: {}x{} on '{}'",
                i, mon.width, height, mon.name,
            );
        }

        let _ = conn.flush();

        let font_desc = pango::FontDescription::from_string(&config.bar.font);

        let font_options = crate::torrentius::make_font_options()?;

        let colors = BarColors {
            bg: parse_hex_local(&config.bar.bg),
            fg: parse_hex_local(&config.bar.fg),
            tag_focused_bg: parse_hex_local(&config.bar.tag_focused_bg),
            tag_focused_fg: parse_hex_local(&config.bar.tag_focused_fg),
            tag_occupied_fg: parse_hex_local(&config.bar.tag_occupied_fg),
            tag_empty_fg: parse_hex_local(&config.bar.tag_empty_fg),
        };

        let path_executables = scan_path();

        eprintln!(
            "[demiurge] bar: {} PATH executables",
            path_executables.len(),
        );

        Ok(Self {
            panels,
            height,
            depth,
            font_desc,
            font_options,
            colors,
            clock_format: config.bar.clock_format.clone(),
            tag_names: tag_names.to_vec(),
            path_executables,
            prompt: None,
            tag_extents: Vec::new(),
        })
    }

    pub fn contains_window(&self, window: Window) -> bool {
        self.panels.iter().any(|p| p.window == window)
    }

    pub fn raise_all(&self, conn: &RustConnection) {
        for panel in &self.panels {
            let _ = conn.configure_window(
                panel.window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );
        }
    }

    pub fn destroy_all(&self, conn: &RustConnection) {
        for panel in &self.panels {
            let _ = conn.destroy_window(panel.window);
        }
    }

    pub fn draw(
        &mut self,
        conn: &RustConnection,
        active_tag: usize,
        occupied: &[bool],
        title: &str,
        _layout: Layout,
    ) {
        for (pi, panel) in self.panels.iter().enumerate() {
            let w = panel.width as i32;
            let h = self.height as i32;

            let (surface, cr) = match crate::torrentius::new_surface(w, h) {
                Ok(sc) => sc,
                Err(_) => continue,
            };

            cr.set_font_options(&self.font_options);

            let (br, bg, bb) = self.colors.bg;
            cr.set_source_rgb(br, bg, bb);
            let _ = cr.paint();

            let layout = pangocairo::functions::create_layout(&cr);
            layout.set_font_description(Some(&self.font_desc));

            let pango_ctx = layout.context();
            pangocairo::functions::context_set_font_options(&pango_ctx, Some(&self.font_options));

            // TAGS (left section)
            if pi == 0 {
                self.tag_extents.clear();
            }
            let mut x = 20i32;
            let tag_count = self.tag_names.len().min(occupied.len());
            for i in 0..tag_count {
                let padded = format!("  {}  ", self.tag_names[i]);
                layout.set_text(&padded);
                let (text_w, text_h) = layout.pixel_size();
                let y = (h - text_h) / 2;

                if i == active_tag {
                    let (fbr, fbg, fbb) = self.colors.tag_focused_bg;
                    cr.set_source_rgb(fbr, fbg, fbb);
                    cr.rectangle(x as f64, 0.0, text_w as f64, h as f64);
                    let _ = cr.fill();
                    let (fr, fg, fb) = self.colors.tag_focused_fg;
                    cr.set_source_rgb(fr, fg, fb);
                } else if occupied[i] {
                    let (or, og, ob) = self.colors.tag_occupied_fg;
                    cr.set_source_rgb(or, og, ob);
                } else {
                    let (er, eg, eb) = self.colors.tag_empty_fg;
                    cr.set_source_rgb(er, eg, eb);
                }

                cr.move_to(x as f64, y as f64);
                pangocairo::functions::show_layout(&cr, &layout);
                let start = x;
                x += text_w;
                if pi == 0 {
                    self.tag_extents.push((start, x));
                }
            }

            let tags_end_x = x;

            // RUN PROMPT (only on first panel)
            let mut prompt_end_x = tags_end_x;
            if pi == 0 {
                if let Some(ref prompt) = self.prompt {
                    prompt_end_x = tags_end_x + 8;

                    let prefix = "> ";
                    let prompt_text = format!("{}{}", prefix, prompt.input);
                    layout.set_text(&prompt_text);
                    let (text_w, text_h) = layout.pixel_size();
                    let y = (h - text_h) / 2;

                    let (fr, fg, fb) = self.colors.fg;
                    cr.set_source_rgb(fr, fg, fb);
                    cr.move_to(prompt_end_x as f64, y as f64);
                    pangocairo::functions::show_layout(&cr, &layout);
                    prompt_end_x += text_w;

                    if let Some(ref comp) = prompt.completion {
                        if comp.len() > prompt.input.len() {
                            let ghost = &comp[prompt.input.len()..];
                            layout.set_text(ghost);
                            let (ghost_w, _) = layout.pixel_size();

                            cr.set_source_rgb(fr * 0.4, fg * 0.4, fb * 0.4);
                            cr.move_to(prompt_end_x as f64, y as f64);
                            pangocairo::functions::show_layout(&cr, &layout);
                            prompt_end_x += ghost_w;
                        }
                    }

                    layout.set_text(&prompt_text[..prefix.len() + prompt.cursor]);
                    let (cursor_x, _) = layout.pixel_size();
                    cr.set_source_rgb(fr, fg, fb);
                    cr.rectangle(
                        (tags_end_x + 8 + cursor_x) as f64,
                        4.0,
                        1.0,
                        (h - 8) as f64,
                    );
                    let _ = cr.fill();
                }
            }

            // CLOCK (right section)
            let clock_text = self.format_clock();
            layout.set_text(&clock_text);
            let (clock_w, clock_h) = layout.pixel_size();
            let clock_x = w - clock_w - 10;
            let clock_y = (h - clock_h) / 2;

            let (fr, fg, fb) = self.colors.fg;
            cr.set_source_rgb(fr, fg, fb);
            cr.move_to(clock_x as f64, clock_y as f64);
            pangocairo::functions::show_layout(&cr, &layout);

            // WINDOW TITLE (center section)
            let title_left = prompt_end_x + 20;
            let title_right = clock_x - 20;
            let title_avail = title_right - title_left;

            if title_avail > 50 && !title.is_empty() {
                layout.set_text(title);
                layout.set_ellipsize(pango::EllipsizeMode::End);
                layout.set_width(title_avail * pango::SCALE);
                let (text_w, text_h) = layout.pixel_size();
                let title_x = title_left + (title_avail - text_w) / 2;
                let title_y = (h - text_h) / 2;

                cr.set_source_rgb(fr, fg, fb);
                cr.move_to(title_x as f64, title_y as f64);
                pangocairo::functions::show_layout(&cr, &layout);

                layout.set_width(-1);
                layout.set_ellipsize(pango::EllipsizeMode::None);
            }

            // Push pixels to X11
            drop(cr);
            surface.flush();

            let _ = surface.with_data(|data| {
                let _ = conn.put_image(
                    ImageFormat::Z_PIXMAP,
                    panel.window,
                    panel.gc,
                    panel.width as u16,
                    self.height as u16,
                    0,
                    0,
                    0,
                    self.depth,
                    data,
                );
            });
        }

        let _ = conn.flush();
    }

    fn format_clock(&self) -> String {
        unsafe {
            let mut t: libc::time_t = 0;
            libc::time(&mut t);
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&t, &mut tm);
            let mut buf = [0u8; 256];
            let fmt = match CString::new(self.clock_format.as_bytes()) {
                Ok(f) => f,
                Err(_) => return String::new(),
            };
            let len = libc::strftime(
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                fmt.as_ptr(),
                &tm,
            );
            String::from_utf8_lossy(&buf[..len]).to_string()
        }
    }

    pub fn start_prompt(&mut self) {
        self.prompt = Some(PromptState {
            input: String::new(),
            cursor: 0,
            completion: None,
        });
    }

    pub fn update_appearance(&mut self, config: &Config) {
        self.colors = BarColors {
            bg: parse_hex_local(&config.bar.bg),
            fg: parse_hex_local(&config.bar.fg),
            tag_focused_bg: parse_hex_local(&config.bar.tag_focused_bg),
            tag_focused_fg: parse_hex_local(&config.bar.tag_focused_fg),
            tag_occupied_fg: parse_hex_local(&config.bar.tag_occupied_fg),
            tag_empty_fg: parse_hex_local(&config.bar.tag_empty_fg),
        };
        self.font_desc = pango::FontDescription::from_string(&config.bar.font);
        self.clock_format = config.bar.clock_format.clone();
    }

    pub fn handle_prompt_key(&mut self, keysym: u32) -> Option<String> {
        let prompt = self.prompt.as_mut()?;

        match keysym {
            0xff0d => {
                // Return/Enter
                if prompt.input.is_empty() {
                    self.prompt = None;
                    return None;
                }
                let cmd = prompt.input.clone();
                self.prompt = None;
                return Some(cmd);
            }
            0xff1b => {
                // Escape
                self.prompt = None;
                return None;
            }
            0xff09 => {
                // Tab: accept completion
                if let Some(ref comp) = prompt.completion {
                    prompt.input = comp.clone();
                    prompt.cursor = prompt.input.len();
                    prompt.completion = find_completion(&self.path_executables, &prompt.input);
                }
            }
            0xff08 => {
                // BackSpace
                if prompt.cursor > 0 {
                    prompt.input.remove(prompt.cursor - 1);
                    prompt.cursor -= 1;
                    prompt.completion = find_completion(&self.path_executables, &prompt.input);
                }
                if prompt.input.is_empty() {
                    prompt.completion = None;
                }
            }
            sym if (0x0020..=0x007e).contains(&sym) => {
                // Printable ASCII
                let ch = sym as u8 as char;
                prompt.input.insert(prompt.cursor, ch);
                prompt.cursor += 1;
                prompt.completion = find_completion(&self.path_executables, &prompt.input);
            }
            _ => {}
        }
        None
    }
}

// Kept public because tests/bar.rs references bar::parse_hex directly.
// Delegates to torrentius where the actual implementation lives.
#[allow(dead_code)]
pub fn parse_hex(color: &str) -> (f64, f64, f64) {
    crate::torrentius::parse_hex(color)
}

fn parse_hex_local(color: &str) -> (f64, f64, f64) {
    crate::torrentius::parse_hex(color)
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
    let idx = executables.partition_point(|s| s.as_str() < input);
    executables
        .get(idx)
        .filter(|s| s.starts_with(input))
        .cloned()
}
