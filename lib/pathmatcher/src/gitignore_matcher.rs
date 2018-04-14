use ignore::Match;
use ignore::gitignore::Gitignore;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Lazy `.gitignore` matcher - load and unload `.gitignore` files on demand.
pub struct GitignoreMatcher {
    ignore: Gitignore,

    // PERF: Each Gitignore object stores "root" as "PathBuf" to support
    // matching against an absolute path. Since we enforce relative path
    // in the API, removing that "PathBuf" could reduce memory footprint.
    submatchers: RefCell<HashMap<PathBuf, Box<GitignoreMatcher>>>,

    // Whether this directory is ignored or not.
    ignored: bool,
}

/// Return (next_component, remaining_path), or None if remaining_path is empty.
fn split_path(path: &Path) -> Option<(&Path, &Path)> {
    let mut comps = path.components();
    let comp = comps.next();
    comp.and_then(|c| {
        let rest = comps.as_path();
        if let Component::Normal(s) = c {
            if rest.as_os_str().is_empty() {
                None
            } else {
                Some((Path::new(s), rest))
            }
        } else {
            panic!("ProgrammingError: unexpected path component {:?}", &c);
        }
    })
}

#[derive(PartialEq)]
enum MatchResult {
    Unspecified,
    Ignored,
    Whitelisted,
}

impl<T> From<Match<T>> for MatchResult {
    fn from(v: Match<T>) -> MatchResult {
        match v {
            Match::None => MatchResult::Unspecified,
            Match::Ignore(_) => MatchResult::Ignored,
            Match::Whitelist(_) => MatchResult::Whitelisted,
        }
    }
}

impl GitignoreMatcher {
    /// Initialize `GitignoreMatch` for the given root directory.
    ///
    /// The `.gitignore` in the root directory will be parsed immediately.
    /// `.gitignore` in subdirectories are parsed lazily.
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        let ignore = Gitignore::new(root.as_ref().join(".gitignore")).0;
        let submatchers = RefCell::new(HashMap::new());
        GitignoreMatcher {
            ignore,
            submatchers,
            ignored: false,
        }
    }

    /// Like `new`, but might mark the subtree as "ignored" entirely.
    /// Used internally by `match_subdir_path`.
    fn new_with_rootmatcher(dir: &Path, root: &GitignoreMatcher) -> Self {
        let dir_root_relative = dir.strip_prefix(root.ignore.path()).unwrap();
        let submatchers = RefCell::new(HashMap::new());
        let (ignored, ignore) = if root.match_relative(dir_root_relative, true) {
            (true, Gitignore::empty())
        } else {
            (false, Gitignore::new(dir.join(".gitignore")).0)
        };
        GitignoreMatcher {
            ignore,
            ignored,
            submatchers,
        }
    }

    /// Return true if the normalized relative path should be ignored.
    ///
    /// Panic if the path is not relative, or contains components like
    /// ".." or ".".
    pub fn match_relative<P: AsRef<Path>>(&self, path: P, is_dir: bool) -> bool {
        let path = path.as_ref();
        self.match_path(path, is_dir, self) == MatchResult::Ignored
    }

    /// Check .gitignore for the relative path.
    fn match_path(&self, path: &Path, is_dir: bool, root: &GitignoreMatcher) -> MatchResult {
        // Everything is ignored regardless if this directory is ignored.
        if self.ignored {
            return MatchResult::Ignored;
        }

        // Check subdir first. It can override this (parent) directory.
        let subdir_result = match split_path(path) {
            None => MatchResult::Unspecified,
            Some((dir, rest)) => self.match_subdir_path(dir, rest, is_dir, root),
        };

        match subdir_result {
            MatchResult::Whitelisted => MatchResult::Whitelisted,
            MatchResult::Ignored => MatchResult::Ignored,
            MatchResult::Unspecified => self.ignore.matched(path, is_dir).into(),
        }
    }

    /// Check .gitignore in the subdirectory `name` for the path `rest`.
    /// Create submatcher on demand.
    fn match_subdir_path(
        &self,
        name: &Path,
        rest: &Path,
        is_dir: bool,
        root: &GitignoreMatcher,
    ) -> MatchResult {
        {
            let submatchers = self.submatchers.borrow();
            if let Some(m) = submatchers.get(name) {
                return m.as_ref().match_path(rest, is_dir, root);
            }
        }
        {
            let dir = self.ignore.path().join(name);
            if dir.is_dir() {
                let m = GitignoreMatcher::new_with_rootmatcher(&dir, root);
                let result = m.match_path(rest, is_dir, root);
                let mut submatchers = self.submatchers.borrow_mut();
                submatchers.insert(name.to_path_buf(), Box::new(m));
                result
            } else {
                MatchResult::Unspecified
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, File};
    use std::io::Write;
    use tempdir::TempDir;

    #[test]
    fn test_split_path() {
        let p = Path::new("proc/self/stat");

        let (c, p) = split_path(p).unwrap();
        assert_eq!(c, Path::new("proc"));
        assert_eq!(p, Path::new("self/stat"));

        let (c, p) = split_path(p).unwrap();
        assert_eq!(c, Path::new("self"));
        assert_eq!(p, Path::new("stat"));

        assert!(split_path(p).is_none());
    }

    #[test]
    fn test_gitignore_match_directory() {
        let dir = TempDir::new("gitignore").expect("tempdir");
        write(dir.path().join(".gitignore"), b"FILE\nDIR/\n");

        let m = GitignoreMatcher::new(dir.path());
        assert!(m.match_relative("x/FILE", false));
        assert!(m.match_relative("x/FILE", true));
        assert!(!m.match_relative("x/DIR", false));
        assert!(m.match_relative("x/DIR", true));
    }

    #[test]
    fn test_gitignore_match_subdir() {
        let dir = TempDir::new("gitignore").expect("tempdir");

        create_dir_all(dir.path().join("a/b")).expect("mkdir");
        create_dir_all(dir.path().join("c/d")).expect("mkdir");
        write(dir.path().join(".gitignore"), b"a/b\n!c/d/*");
        write(dir.path().join("a/b/.gitignore"), b"!c");
        write(dir.path().join("a/.gitignore"), b"!b/d");
        write(dir.path().join("c/.gitignore"), b"d/e\n!d/f");
        write(dir.path().join("c/d/.gitignore"), b"!e\nf");

        let m = GitignoreMatcher::new(dir.path());
        assert!(m.match_relative("a/b", false));
        assert!(m.match_relative("a/b/c", false));
        assert!(m.match_relative("a/b/d", false));
        assert!(m.match_relative("c/d/f", false));
        assert!(!m.match_relative("c/d/e", false));
    }

    fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) {
        File::create(path)
            .expect("create")
            .write_all(contents.as_ref())
            .expect("write");
    }
}
