//! Git status of a folder's entries (for badges): one `git status` for the repository, mapped to direct children.
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// Status letter per child name: U conflict, M modified, D deleted, A added, ? untracked. None outside a repo.
pub fn status(dir: &Path) -> Option<HashMap<OsString, char>> {
    // Never run repo-controlled helpers: a malicious clone/share could set core.fsmonitor to RCE on open.
    let helper: &[&str] = &["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"];
    let prefix = Command::new("git").arg("-C").arg(dir).args(helper).args(["rev-parse", "--show-prefix"]).env("GIT_OPTIONAL_LOCKS", "0").output().ok().filter(|o| o.status.success())?;
    let prefix = String::from_utf8_lossy(&prefix.stdout).trim().to_string();
    let out = Command::new("git").arg("-C").arg(dir).args(helper).args(["status", "--porcelain=v1", "-z", "--untracked-files=normal", "--", "."]).env("GIT_OPTIONAL_LOCKS", "0").output().ok()?;
    let rank = |c: char| match c {
        'U' => 5,
        'M' => 4,
        'D' => 3,
        'A' => 2,
        _ => 1,
    };
    let mut map: HashMap<OsString, char> = HashMap::new();
    let mut records = out.stdout.split(|&b| b == 0).filter(|r| r.len() > 3);
    while let Some(rec) = records.next() {
        let (xy, path) = (&rec[..2], String::from_utf8_lossy(&rec[3..]).into_owned());
        if xy[0] == b'R' || xy[0] == b'C' {
            records.next(); // the old name of a rename/copy
        }
        let c = match (xy[0], xy[1]) {
            (b'?', _) => '?',
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => 'U',
            (b'A', _) => 'A',
            (b'D', _) | (_, b'D') => 'D',
            _ => 'M',
        };
        let Some(rest) = path.strip_prefix(&prefix) else { continue };
        let child = rest.split('/').next().unwrap_or("");
        if child.is_empty() {
            continue;
        }
        // Folders show the strongest status inside them.
        let slot = map.entry(OsString::from(child)).or_insert(c);
        if rank(c) > rank(*slot) {
            *slot = c;
        }
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    #[test]
    fn repo_status() {
        let root = std::env::temp_dir().join(format!("kolphin-git-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(&root).args(args).output().unwrap();
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), "1").unwrap();
        std::fs::write(root.join("sub/b.txt"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        std::fs::write(root.join("a.txt"), "2").unwrap();
        std::fs::write(root.join("sub/new.txt"), "1").unwrap();
        let m = super::status(&root).unwrap();
        assert_eq!(m.get(std::ffi::OsStr::new("a.txt")), Some(&'M'));
        assert_eq!(m.get(std::ffi::OsStr::new("sub")), Some(&'?'));
        let sub = super::status(&root.join("sub")).unwrap();
        assert_eq!(sub.get(std::ffi::OsStr::new("new.txt")), Some(&'?'));
        assert!(super::status(&std::env::temp_dir()).is_none() || !std::env::temp_dir().join(".git").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
