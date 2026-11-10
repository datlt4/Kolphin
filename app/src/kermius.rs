//! Drives Kermius over its control socket (docs/kermius-control-api.md): Kolphin's own terminal tab that follows
//! the folder, program tabs (Claude, OpenCode), sharing. Without Kermius, falls back to the default terminal.
use gtk::glib;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

const KEY: &str = "kolphin";

type Reply = Box<dyn FnOnce(Result<Value, String>)>;
type Waiter = Box<dyn FnOnce(bool)>;
type CwdHandler = Box<dyn Fn(PathBuf)>;

#[derive(Default)]
pub struct Kermius {
    stream: RefCell<Option<UnixStream>>,
    next_id: Cell<u64>,
    pending: RefCell<HashMap<u64, Reply>>,
    /// Kolphin's terminal tab, while it is open.
    session: RefCell<Option<String>>,
    /// Waiting for Kermius to start: calls queue here.
    waiting: RefCell<Vec<Waiter>>,
    /// The shell in Kolphin's tab changed directory.
    pub on_cwd: RefCell<Option<CwdHandler>>,
}

thread_local! {
    static K: Rc<Kermius> = Rc::new(Kermius::default());
}

pub fn get() -> Rc<Kermius> {
    K.with(Rc::clone)
}

fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("kermius/control.sock"),
        None => PathBuf::from(format!("/tmp/kermius-{}/control.sock", crate::ops::uid())),
    }
}

/// Refuse a socket owned by someone else, world-writable, or a symlink: a fake
/// Kermius could otherwise steal cwd/argv/paths or inject fake cwd events.
fn socket_ok(p: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(meta) = std::fs::symlink_metadata(p) else { return false };
    if meta.file_type().is_symlink() {
        return false;
    }
    if meta.uid() != crate::ops::uid() {
        return false;
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return false;
    }
    if let Some(dir) = p.parent() {
        if let Ok(dm) = std::fs::symlink_metadata(dir) {
            if dm.file_type().is_symlink() || dm.uid() != crate::ops::uid() {
                return false;
            }
            if dm.permissions().mode() & 0o077 != 0 {
                return false;
            }
        }
    }
    true
}

pub fn installed() -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("kermius").is_file()))
}

/// Kolphin's logo for the Kermius tab: the installed icon, or the embedded one written to the cache.
fn icon_path() -> String {
    let installed = Path::new("/usr/share/icons/hicolor/256x256@2/apps/kolphin.png");
    if installed.exists() {
        return installed.display().to_string();
    }
    let p = glib::user_cache_dir().join("kolphin/kolphin.png");
    if !p.exists() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(&p, include_bytes!("../../assets/icons/kolphin-256.png"));
    }
    p.display().to_string()
}

/// Tab mark per coding tool (file name, SVG bytes). Brand glyphs are CC0 Simple Icons;
/// codex.svg is drawn here (no brand asset shipped).
fn tool_art(prog: &str) -> Option<(&'static str, &'static [u8])> {
    match prog {
        "claude" | "claude-code" => Some(("tool-claude.svg", include_bytes!("../../assets/tools/claude.svg"))),
        "opencode" => Some(("tool-opencode.svg", include_bytes!("../../assets/tools/opencode.svg"))),
        "codex" | "codex-cli" => Some(("tool-codex.svg", include_bytes!("../../assets/tools/codex.svg"))),
        _ => None,
    }
}

/// Icon for a `session.run` tab: the tool's own mark when known, else Kolphin's logo.
/// The basename of argv[0] decides (`~/.local/bin/claude` is still Claude).
fn run_icon(argv: &[String]) -> String {
    let prog = argv.first().map(|a| a.rsplit('/').next().unwrap_or(a).to_lowercase()).unwrap_or_default();
    if let Some((file, art)) = tool_art(prog.as_str()) {
        let p = glib::user_cache_dir().join("kolphin").join(file);
        if !p.exists() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(&p, art);
        }
        if p.is_file() {
            return p.display().to_string();
        }
    }
    icon_path()
}

/// Title for a `session.run` tab: just the folder (the icon already says which tool).
fn run_title(cwd: &Path) -> String {
    cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
}

impl Kermius {
    /// Run `f(true)` once connected (starting Kermius in the background if needed), or `f(false)`.
    fn ensure(self: &Rc<Self>, f: impl FnOnce(bool) + 'static) {
        if self.stream.borrow().is_some() || self.connect() {
            return f(true);
        }
        if !installed() {
            return f(false);
        }
        self.waiting.borrow_mut().push(Box::new(f));
        if self.waiting.borrow().len() > 1 {
            return;
        }
        let _ = std::process::Command::new("kermius").arg("--background").spawn();
        let me = self.clone();
        let tries = Cell::new(0);
        glib::timeout_add_local(Duration::from_millis(300), move || {
            tries.set(tries.get() + 1);
            let ok = me.connect();
            if !ok && tries.get() < 34 {
                return glib::ControlFlow::Continue;
            }
            for f in me.waiting.take() {
                f(ok);
            }
            glib::ControlFlow::Break
        });
    }

    fn connect(self: &Rc<Self>) -> bool {
        let path = socket_path();
        if !socket_ok(&path) {
            return false;
        }
        let Ok(stream) = UnixStream::connect(path) else { return false };
        let Ok(reader) = stream.try_clone() else { return false };
        *self.stream.borrow_mut() = Some(stream);
        let (tx, rx) = async_channel::unbounded::<Value>();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str(&line) {
                    if tx.send_blocking(v).is_err() {
                        break;
                    }
                }
            }
        });
        let me = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                let Some(k) = me.upgrade() else { return };
                k.dispatch(msg);
            }
            // Kermius quit: every session is gone.
            if let Some(k) = me.upgrade() {
                k.stream.take();
                k.session.take();
                for (_, cb) in k.pending.take() {
                    cb(Err("Kermius closed".into()));
                }
            }
        });
        self.call("hello", json!({"client": "kolphin", "version": env!("CARGO_PKG_VERSION")}), |_| {});
        true
    }

    fn dispatch(&self, msg: Value) {
        if let Some(id) = msg["id"].as_u64() {
            if let Some(cb) = self.pending.borrow_mut().remove(&id) {
                cb(match msg.get("error") {
                    Some(e) => Err(e["code"].as_str().unwrap_or("error").to_string()),
                    None => Ok(msg["result"].clone()),
                });
            }
            return;
        }
        let p = &msg["params"];
        let ours = p["key"].as_str() == Some(KEY);
        match msg["event"].as_str() {
            Some("session.closed") if ours => {
                self.session.take();
            }
            Some("session.cwd") if ours => {
                if let (Some(cwd), Some(f)) = (p["cwd"].as_str(), self.on_cwd.borrow().as_ref()) {
                    f(PathBuf::from(cwd));
                }
            }
            _ => {}
        }
    }

    fn call(&self, method: &str, params: Value, cb: impl FnOnce(Result<Value, String>) + 'static) {
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        let line = json!({"id": id, "method": method, "params": params}).to_string() + "\n";
        let sent = self.stream.borrow_mut().as_mut().is_some_and(|s| s.write_all(line.as_bytes()).is_ok());
        if sent {
            self.pending.borrow_mut().insert(id, Box::new(cb));
        } else {
            self.stream.take();
            cb(Err("not connected".into()));
        }
    }

    pub fn has_terminal(&self) -> bool {
        self.session.borrow().is_some()
    }

    /// Open (or focus) Kolphin's terminal tab in `cwd`.
    pub fn open_terminal(self: &Rc<Self>, cwd: &Path) {
        let cwd = cwd.to_path_buf();
        let me = self.clone();
        self.ensure(move |ok| {
            if !ok {
                return fallback_terminal(&cwd, None);
            }
            let params = json!({"key": KEY, "cwd": cwd, "title": "Kolphin", "icon": icon_path(), "focus": true});
            let k = me.clone();
            me.call("session.open", params, move |r| match r {
                Ok(v) => *k.session.borrow_mut() = v["session"].as_str().map(String::from),
                Err(_) => fallback_terminal(&cwd, None),
            });
        });
    }

    /// Follow the view: cd Kolphin's tab (only if it is open; Kermius skips it while a program runs).
    pub fn cd(self: &Rc<Self>, path: &Path) {
        let Some(session) = self.session.borrow().clone() else { return };
        let k = self.clone();
        self.call("session.cd", json!({"session": session, "path": path}), move |r| {
            if r.err().as_deref() == Some("not_found") {
                k.session.take();
            }
        });
    }

    /// A new tab running `argv` in `cwd` (Claude, OpenCode, …): titled with the folder, badged with the tool.
    pub fn run(self: &Rc<Self>, argv: Vec<String>, cwd: &Path) {
        let (cwd, title, icon) = (cwd.to_path_buf(), run_title(cwd), run_icon(&argv));
        let me = self.clone();
        self.ensure(move |ok| {
            if !ok {
                return fallback_terminal(&cwd, Some(&argv));
            }
            let params = json!({"argv": argv, "cwd": cwd, "title": title, "icon": icon, "focus": true});
            me.call("session.run", params, move |r| {
                if r.is_err() {
                    fallback_terminal(&cwd, Some(&argv));
                }
            });
        });
    }

    /// Hand files to Kermius' Nearby Share.
    pub fn share(self: &Rc<Self>, paths: Vec<PathBuf>) -> bool {
        if !installed() {
            return false;
        }
        let me = self.clone();
        self.ensure(move |ok| {
            if ok {
                me.call("share", json!({"paths": paths}), |_| {});
            } else {
                let _ = std::process::Command::new("kermius").arg("--share").args(&paths).spawn();
            }
        });
        true
    }
}

/// No Kermius: the desktop's terminal, with a hint to install it.
fn fallback_terminal(cwd: &Path, argv: Option<&[String]>) {
    let hint = "echo 'Tip: install Kermius so Kolphin can open and drive its terminal tabs.'";
    let script = match argv {
        Some(a) => format!("{hint}; {}; exec bash", a.iter().map(|s| glib::shell_quote(s).to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")),
        None => format!("{hint}; exec bash"),
    };
    let r = std::process::Command::new("gnome-terminal").arg(format!("--working-directory={}", cwd.display())).args(["--", "bash", "-c", &script]).spawn();
    if r.is_err() {
        let _ = std::process::Command::new("x-terminal-emulator").current_dir(cwd).args(["-e", "bash", "-c", &script]).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_marks() {
        assert!(tool_art("claude").is_some());
        assert!(tool_art("claude-code").is_some());
        assert!(tool_art("opencode").is_some());
        assert!(tool_art("codex").is_some());
        assert!(tool_art("codex-cli").is_some());
        assert!(tool_art("vim").is_none());
        for (_, art) in ["claude", "opencode", "codex"].iter().filter_map(|t| tool_art(t)) {
            assert!(!art.is_empty());
        }
    }

    #[test]
    fn run_tabs_show_folders() {
        assert_eq!(run_title(Path::new("/home/koi/Kolphin")), "Kolphin");
        assert_eq!(run_title(Path::new("/")), "/");
        assert_eq!(run_title(Path::new("proj")), "proj");
    }
}
