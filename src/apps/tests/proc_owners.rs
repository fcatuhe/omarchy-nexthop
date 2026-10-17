use super::super::owners::{Owner, Owners, comm_name, socket_inode};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

struct FakeProc {
    root: PathBuf,
}

impl FakeProc {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nexthop-apps-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn process(&self, pid: i32, comm: &str, fds: &[(i32, &str)]) {
        let dir = self.root.join(pid.to_string());
        fs::create_dir_all(dir.join("fd")).unwrap();
        fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        for (fd, target) in fds {
            symlink(target, dir.join("fd").join(fd.to_string())).unwrap();
        }
    }

    fn kill(&self, pid: i32) {
        fs::remove_dir_all(self.root.join(pid.to_string())).unwrap();
    }

    fn owners(&self) -> Owners {
        Owners::with_root(self.root.to_str().unwrap())
    }
}

impl Drop for FakeProc {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const FAR: f64 = f64::MAX;

fn owner(pid: i32, name: &str) -> Option<Owner> {
    Some(Owner { pid, name: name.to_string() })
}

#[test]
fn socket_links_and_comm_parse_like_ss() {
    assert_eq!(socket_inode(b"socket:[4242]"), Some(4242));
    assert_eq!(socket_inode(b"pipe:[4242]"), None);
    assert_eq!(socket_inode(b"/dev/null"), None);
    assert_eq!(comm_name(b"chrome\n"), "chrome");
    // ss reads the name from /proc/<pid>/stat with "(%[^)]", so it stops at the first ')'
    assert_eq!(comm_name(b"a\"b)c d\n"), "a\"b");
    assert_eq!(comm_name(b")x\n"), "");
    assert_eq!(comm_name(b"two\nlines\n"), "two\nlines");
    assert_eq!(comm_name(b"bad\xffbyte\n"), "bad\u{fffd}byte");
}

#[test]
fn resolves_owners_from_proc_fds() {
    let p = FakeProc::new("resolve");
    p.process(100, "curl", &[(3, "socket:[555]"), (4, "/dev/null"), (5, "socket:[999]")]);
    p.process(200, "brave", &[(7, "socket:[556]")]);
    fs::create_dir_all(p.root.join("300")).unwrap();
    let mut o = p.owners();
    assert!(o.resolve([555, 556, 557, 0], FAR));
    assert_eq!(o.get(555).cloned(), owner(100, "curl"));
    assert_eq!(o.get(556).cloned(), owner(200, "brave"));
    assert_eq!(o.get(557), None);
    assert_eq!(o.get(999), None);
    assert_eq!(o.scans, 1);
}

#[test]
fn shared_socket_goes_to_the_last_pid_and_fd_scanned_like_ss() {
    let p = FakeProc::new("shared");
    p.process(99, "parent", &[(3, "socket:[777]")]);
    p.process(1000, "child", &[(4, "socket:[777]"), (12, "socket:[778]"), (9, "socket:[778]")]);
    let mut o = p.owners();
    o.resolve([777, 778], FAR);
    assert_eq!(o.get(777).cloned(), owner(1000, "child"));
}

#[test]
fn rescans_only_when_an_unknown_inode_appears() {
    let p = FakeProc::new("rescan");
    p.process(100, "curl", &[(3, "socket:[555]")]);
    let mut o = p.owners();
    o.resolve([555, 557], FAR);
    o.resolve([555, 557], FAR);
    o.resolve([555], FAR);
    assert_eq!(o.scans, 1);
    p.process(200, "brave", &[(7, "socket:[556]")]);
    o.resolve([555, 556], FAR);
    assert_eq!(o.scans, 2);
    assert_eq!(o.get(556).cloned(), owner(200, "brave"));
}

#[test]
fn stale_inodes_are_dropped_and_resolved_again_if_seen_later() {
    let p = FakeProc::new("stale");
    p.process(100, "curl", &[(3, "socket:[555]")]);
    let mut o = p.owners();
    o.resolve([555], FAR);
    o.resolve([], FAR);
    assert_eq!(o.get(555), None);
    o.resolve([555], FAR);
    assert_eq!(o.scans, 2);
    assert_eq!(o.get(555).cloned(), owner(100, "curl"));
}

#[test]
fn exited_owner_triggers_a_rescan_that_finds_the_heir() {
    let p = FakeProc::new("heir");
    p.process(100, "parent", &[(3, "socket:[555]")]);
    let mut o = p.owners();
    o.resolve([555], FAR);
    p.kill(100);
    p.process(150, "heir", &[(5, "socket:[555]")]);
    o.resolve([555], FAR);
    assert_eq!(o.scans, 2);
    assert_eq!(o.get(555).cloned(), owner(150, "heir"));
}

#[test]
fn renamed_owner_is_picked_up_without_a_scan() {
    let p = FakeProc::new("rename");
    p.process(100, "before", &[(3, "socket:[555]")]);
    let mut o = p.owners();
    o.resolve([555], FAR);
    fs::write(p.root.join("100/comm"), "after\n").unwrap();
    o.resolve([555], FAR);
    assert_eq!(o.scans, 1);
    assert_eq!(o.get(555).cloned(), owner(100, "after"));
}

#[test]
fn scan_past_the_deadline_keeps_unresolved_inodes_unknown() {
    let p = FakeProc::new("deadline");
    p.process(100, "curl", &[(3, "socket:[555]")]);
    let mut o = p.owners();
    assert!(!o.resolve([555], 0.0));
    assert_eq!(o.get(555), None);
    assert!(o.resolve([555], FAR));
    assert_eq!(o.scans, 2);
    assert_eq!(o.get(555).cloned(), owner(100, "curl"));
}

#[test]
fn a_new_socket_of_a_known_owner_is_found_without_a_full_scan() {
    let p = FakeProc::new("hot");
    p.process(100, "brave", &[(3, "socket:[555]")]);
    p.process(200, "curl", &[(4, "socket:[600]")]);
    let mut o = p.owners();
    o.resolve([555], FAR);
    p.process(100, "brave", &[(8, "socket:[556]")]);
    assert!(o.resolve([555, 556], FAR));
    assert_eq!(o.scans, 1);
    assert_eq!(o.get(556).cloned(), owner(100, "brave"));
    o.resolve([555, 556, 600], FAR);
    assert_eq!(o.scans, 2);
    assert_eq!(o.get(600).cloned(), owner(200, "curl"));
}
