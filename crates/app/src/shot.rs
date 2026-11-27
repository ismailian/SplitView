//! `--shot <file.png> [--scene <name>]`: build a scene, capture the window
//! through egui's own screenshot command (works without a visible desktop),
//! write a PNG and exit. Used for the README/site screenshots and release
//! checks. Scenes: `main`, `settings`, `palette`, `background`, `picker`,
//! `shell`, `mcp`, `zoom`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sv_core::layout::Direction;

use crate::app::{App, Focus};
use crate::dialogs::Modal;
use crate::settings_ui::Section;

pub struct Shot {
    pub out: PathBuf,
    pub scene: String,
    start: Instant,
    step: u32,
    requested: bool,
}

impl Shot {
    pub fn new(out: PathBuf, scene: String) -> Self {
        Self {
            out,
            scene,
            start: Instant::now(),
            step: 0,
            requested: false,
        }
    }

    /// Save the screenshot when it arrives, then quit.
    pub fn tick(&mut self, ctx: &egui::Context) {
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = image {
            let [w, h] = img.size;
            let mut rgba = Vec::with_capacity(w * h * 4);
            for p in &img.pixels {
                rgba.extend_from_slice(&[p.r(), p.g(), p.b(), 255]);
            }
            if let Err(e) = std::fs::write(&self.out, png(w as u32, h as u32, &rgba)) {
                eprintln!("shot: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

/// Type a command into a pane. It starts with Ctrl+U (clear the line): a
/// shell that is still settling can drop the first byte of scripted input,
/// and losing that one costs nothing.
fn type_in(app: &mut App, wi: usize, pane_index: usize, text: &str) {
    let leaves = app.windows[wi].layout.leaves();
    if let Some(id) = leaves.get(pane_index) {
        let _ = app.hub.write(*id, format!("\x15{text}").as_bytes());
    }
}

/// Advance the scripted scene.
pub fn drive(app: &mut App, ctx: &egui::Context) {
    let Some(shot) = app.shot.as_ref() else {
        return;
    };
    if app.windows[0].layout.is_empty() && shot.scene != "picker" {
        return;
    }
    let t = shot.start.elapsed();
    let step = shot.step;
    let scene = shot.scene.clone();
    let cwd = std::env::var("SPLITVIEW_SHOT_CWD").ok();
    let next = |app: &mut App| {
        if let Some(s) = app.shot.as_mut() {
            s.step += 1;
        }
    };
    match step {
        0 if t > Duration::from_millis(1600) => {
            if scene == "picker" {
                next(app);
                return;
            }
            if let Some(c) = &cwd {
                type_in(app, 0, 0, &format!("cd '{c}'\r"));
            }
            next(app);
        }
        1 if t > Duration::from_millis(2200) => {
            if scene != "picker" {
                let _ = app.split_into(0, Direction::Right, cwd.clone(), None, true);
                let _ = app.split_into(0, Direction::Down, cwd.clone(), None, true);
                let w = &mut app.windows[0];
                w.layout = w.layout.set_ratio(&[], 0.56);
                let first = w.layout.leaves()[0];
                w.layout = w.layout.focus(first);
            }
            next(app);
        }
        2 if t > Duration::from_millis(4000) => {
            if scene != "picker" {
                let first = if scene == "background" {
                    // Something worth backgrounding: a dev server that keeps running.
                    "clear; echo 'VITE v6.0.3  ready in 312 ms'; echo '  Local:   http://localhost:5173/'; sleep 3600\r"
                } else {
                    "clear; git --no-pager log --oneline --graph --decorate -n 12 --color=always\r"
                };
                type_in(app, 0, 0, first);
                type_in(app, 0, 1, "ls -lgGhA --color=always\r");
                type_in(
                    app,
                    0,
                    2,
                    "cargo test --release -p splitview-core -q 2>&1 | tail -n 6\r",
                );
                let w = &mut app.windows[0];
                w.sidebar.set_open(true, &app.hub);
                w.focus = Focus::Terminal;
                app.sync_sidebar(0);
                if let Some(c) = &cwd {
                    let file = format!("{c}/crates/core/src/layout.rs");
                    app.open_in_editor(0, &file);
                    app.windows[0].focus = Focus::Terminal;
                }
            }
            next(app);
        }
        3 if t > Duration::from_millis(6400) => {
            let key = app.windows[0].key;
            match scene.as_str() {
                "settings" => app.modal = Some((key, Modal::settings())),
                "shell" => app.modal = Some((key, Modal::settings_at(Section::Shell))),
                "mcp" => {
                    app.apply_mcp(); // per settings.json: on only if enabled there
                    app.modal = Some((key, Modal::settings_at(Section::Mcp)));
                }
                "palette" => {
                    app.modal = Some((
                        key,
                        Modal::Palette {
                            query: "split".into(),
                            sel: 0,
                            focus: true,
                        },
                    ))
                }
                "background" => {
                    app.run_action(0, "background_pane");
                    app.modal = Some((key, Modal::Background { sel: 0 }));
                }
                "zoom" => app.run_action(0, "toggle_zoom"),
                "scratch" => {
                    app.run_action(0, "toggle_scratchpad");
                    let cwd = app.focused_pane(0).and_then(|p| p.cwd.clone());
                    app.scratch.demo(cwd.as_deref());
                    app.claude = crate::claude::Usage::demo(cwd.unwrap_or_default());
                }
                _ => {}
            }
            next(app);
        }
        4 if t > Duration::from_millis(7600) => {
            if let Some(s) = app.shot.as_mut()
                && !s.requested
            {
                s.requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            next(app);
        }
        _ => {}
    }
    if step > 4 && t > Duration::from_secs(20) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// A minimal PNG encoder (stored deflate blocks): no image crate needed.
fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (n, t) in table.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *t = c;
        }
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c = table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
        }
        c ^ 0xffff_ffff
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * w as usize * 4..(y + 1) * w as usize * 4]);
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn png_has_signature_and_chunks() {
        let p = super::png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert!(p.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(p.windows(4).any(|w| w == b"IHDR"));
        assert!(p.ends_with(&[0xae, 0x42, 0x60, 0x82]));
    }
}
