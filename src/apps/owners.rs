use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::os::unix::ffi::OsStrExt;

use crate::util::monotonic;

#[derive(Clone, Debug, PartialEq)]
pub struct Owner {
    pub pid: i32,
    pub name: String,
}

pub struct Owners {
    root: String,
    by_inode: HashMap<u32, Option<Owner>>,
    #[cfg(test)]
    pub scans: u64,
}

impl Default for Owners {
    fn default() -> Self {
        Self::new()
    }
}

pub fn socket_inode(link: &[u8]) -> Option<u32> {
    let digits = link.strip_prefix(b"socket:[")?.strip_suffix(b"]")?;
    std::str::from_utf8(digits).ok()?.parse().ok()
}

pub fn comm_name(comm: &[u8]) -> String {
    let comm = comm.strip_suffix(b"\n").unwrap_or(comm);
    let end = comm.iter().position(|&c| c == b')').unwrap_or(comm.len());
    String::from_utf8_lossy(&comm[..end]).into_owned()
}

fn numeric_entries(dir: &str) -> Vec<i32> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<i32> = entries.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect();
    out.sort_unstable();
    out
}

impl Owners {
    pub fn new() -> Self {
        Self::with_root("/proc")
    }

    pub fn with_root(root: &str) -> Self {
        Self {
            root: root.trim_end_matches('/').to_string(),
            by_inode: HashMap::new(),
            #[cfg(test)]
            scans: 0,
        }
    }

    pub fn get(&self, inode: u32) -> Option<&Owner> {
        self.by_inode.get(&inode)?.as_ref()
    }

    pub fn resolve(&mut self, inodes: impl IntoIterator<Item = u32>, deadline: f64) -> bool {
        let live: HashSet<u32> = inodes.into_iter().filter(|&i| i != 0).collect();
        self.by_inode.retain(|ino, _| live.contains(ino));
        self.refresh_names();
        let mut unknown: HashSet<u32> = live.iter().copied().filter(|i| !self.by_inode.contains_key(i)).collect();
        if unknown.is_empty() {
            return true;
        }
        let known_owners: BTreeSet<i32> = self.by_inode.values().flatten().map(|o| o.pid).collect();
        let mut path = String::new();
        for pid in known_owners {
            if monotonic() > deadline {
                break;
            }
            for (ino, owner) in self.sockets_of(pid, &unknown, &mut path) {
                unknown.remove(&ino);
                self.by_inode.insert(ino, Some(owner));
            }
        }
        if unknown.is_empty() {
            return true;
        }
        self.scan(&live, deadline)
    }

    fn sockets_of(&self, pid: i32, wanted: &HashSet<u32>, path: &mut String) -> Vec<(u32, Owner)> {
        path.clear();
        let _ = write!(path, "{}/{pid}/fd/", self.root);
        let base = path.len();
        let mut name: Option<String> = None;
        let mut found = Vec::new();
        for fd in numeric_entries(&path[..base - 1]) {
            path.truncate(base);
            let _ = write!(path, "{fd}");
            let Ok(link) = fs::read_link(&*path) else {
                continue;
            };
            let Some(ino) = socket_inode(link.as_os_str().as_bytes()) else {
                continue;
            };
            if !wanted.contains(&ino) {
                continue;
            }
            let name = name.get_or_insert_with(|| self.read_comm(pid).unwrap_or_default());
            found.push((ino, Owner { pid, name: name.clone() }));
        }
        found
    }

    fn read_comm(&self, pid: i32) -> Option<String> {
        fs::read(format!("{}/{pid}/comm", self.root)).ok().map(|raw| comm_name(&raw))
    }

    fn refresh_names(&mut self) {
        let mut names: HashMap<i32, Option<String>> = HashMap::new();
        for owner in self.by_inode.values().flatten() {
            names.entry(owner.pid).or_insert_with(|| self.read_comm(owner.pid));
        }
        self.by_inode.retain(|_, slot| match slot {
            None => true,
            Some(owner) => match &names[&owner.pid] {
                Some(name) => {
                    owner.name.clone_from(name);
                    true
                }
                None => false,
            },
        });
    }

    fn scan(&mut self, live: &HashSet<u32>, deadline: f64) -> bool {
        #[cfg(test)]
        {
            self.scans += 1;
        }
        let mut found: HashMap<u32, Owner> = HashMap::new();
        let mut complete = true;
        let mut path = String::new();
        for pid in numeric_entries(&self.root) {
            if monotonic() > deadline {
                complete = false;
                break;
            }
            found.extend(self.sockets_of(pid, live, &mut path));
        }
        if complete {
            for &ino in live {
                self.by_inode.insert(ino, found.remove(&ino));
            }
        } else {
            for (ino, owner) in found {
                self.by_inode.insert(ino, Some(owner));
            }
        }
        complete
    }
}
