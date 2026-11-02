//! Custom actions from ~/.config/kolphin/actions.toml: right-click menu and command palette entries.
use serde::Deserialize;
use std::path::{Path, PathBuf};

const DEFAULT: &str = r#"# Kolphin custom actions: shown in the right-click menu and the command palette (Ctrl+K).
# Placeholders in `command`:
#   {dir}    the folder being shown
#   {target} the selected folder if exactly one is selected, else {dir}
#   {file}   the first selected item
#   {files}  every selected item, one argument each
# terminal = true runs the command in a new Kermius tab (or the default terminal).

[[action]]
name = "Open in VS Code"
command = ["code", "{target}"]

[[action]]
name = "Open Claude Code here"
command = ["claude"]
terminal = true

[[action]]
name = "Open OpenCode here"
command = ["opencode"]
terminal = true
"#;

#[derive(Deserialize, Clone)]
pub struct Action {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub terminal: bool,
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    action: Vec<Action>,
}

pub fn config_file() -> PathBuf {
    gtk::glib::user_config_dir().join("kolphin/actions.toml")
}

/// The configured actions (the file is created with the defaults the first time).
pub fn load() -> Vec<Action> {
    let path = config_file();
    if !path.exists() {
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let _ = std::fs::write(&path, DEFAULT);
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT.into());
    toml::from_str::<File>(&text).map(|f| f.action).unwrap_or_default()
}

/// The argv with placeholders filled in, and the folder to run it in.
pub fn expand(a: &Action, dir: &Path, selected: &[PathBuf]) -> (Vec<String>, PathBuf) {
    let target = match selected {
        [one] if one.is_dir() => one.clone(),
        _ => dir.to_path_buf(),
    };
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut argv = Vec::new();
    for arg in &a.command {
        if arg == "{files}" {
            argv.extend(selected.iter().map(|p| s(p)));
        } else {
            let first = selected.first().map(|p| s(p)).unwrap_or_default();
            argv.push(arg.replace("{dir}", &s(dir)).replace("{target}", &s(&target)).replace("{file}", &first));
        }
    }
    (argv, target)
}

/// Run an action: in a Kermius tab when `terminal`, else detached.
pub fn run(a: &Action, dir: &Path, selected: &[PathBuf]) -> Result<(), String> {
    let (argv, cwd) = expand(a, dir, selected);
    let Some((prog, args)) = argv.split_first() else { return Err("empty command".into()) };
    if a.terminal {
        crate::kermius::get().run(argv.clone(), &cwd, &a.name);
        return Ok(());
    }
    std::process::Command::new(prog).args(args).current_dir(&cwd).spawn().map(|_| ()).map_err(|e| format!("{prog}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_expand() {
        let acts = toml::from_str::<File>(DEFAULT).unwrap().action;
        assert_eq!(acts.len(), 3);
        assert!(acts[1].terminal && !acts[0].terminal);
        let dir = std::env::temp_dir();
        let (argv, cwd) = expand(&acts[0], &dir, &[]);
        assert_eq!(argv, ["code".to_string(), dir.to_string_lossy().into_owned()]);
        assert_eq!(cwd, dir);
        let a = Action { name: "x".into(), command: vec!["echo".into(), "{files}".into(), "{file}".into()], terminal: false };
        let (argv, _) = expand(&a, &dir, &[PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(argv, ["echo", "/a", "/b", "/a"]);
    }
}
