// Copyright Facebook, Inc. 2019
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Here we have types for working with paths specialized for source control internals.
//! They are akin to str and String in high level behavior. `RepoPath` is an unsized type wrapping
//! a str so it can't be instantiated directly. `RepoPathBuf` represents the owned version of a
//! RepoPath and wraps a String.
//!
//! The inspiration for `RepoPath` and `RepoPathBuf` comes from the std::path crate however
//! we know that the internal representation of a path is consistently a utf8 string where
//! directories are delimited by the `SEPARATOR` (`/`) so our types can have a simpler
//! representation. It is because of the same reason that we can't use the abstractions in
//! `std::path` for internal uses where we need to apply the same algorithm for blobs we get from
//! the server across all systems.
//!
//! We could use `String` and `&str` directly however these types are inexpressive and have few
//! guarantees. Rust has a strong type system so we can leverage it to provide more safety.
//!
//! `PathComponent` and `PathComponentBuf` can be seen as specializations of `RepoPath` and
//! `RepoPathBuf` that do not have any `SEPARATOR` characters. The main operation that is done on
//! paths is iterating over its components. `PathComponents` are names of files or directories.
//! For the path: `foo/bar/baz.txt` we have 3 components: `foo`, `bar` and `baz.txt`.
//!
//! A lot of algorithms used in source control management operate on directories so having an
//! abstraction for individual components is going to increase readability in the long run.
//! A clear example for where we may want to use `PathComponentBuf` is in the treemanifest logic
//! where all indexing is done using components. The index in those cases must be able to own
//! component. Writing it in terms of `RepoPathBuf` would probably be less readable that
//! writing it in terms of `String`.

use std::{
    borrow::{Borrow, ToOwned},
    convert::AsRef,
    fmt, mem,
    ops::Deref,
};

use failure::{bail, Fallible};

/// An owned version of a `RepoPath`.
#[derive(Clone, Debug, Default, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct RepoPathBuf(String);

/// A normalized path starting from the root of the repository. Paths can be broken into
/// components by using `SEPARATOR`. Normalized means that it following the following rules:
///  * unicode is normalized
///  * does not end with a `SEPARATOR`
///  * does not contain:
///    * \0, null character - it is an illegal file name character on unix
///    * \1, CTRL-A - used as metadata separator
///    * \10, newline - used as metadata separator
///  * does not contain the following components:
///    * ``, empty, implies that paths can't start with, end or contain consecutive `SEPARATOR`s
///    * `.`, dot/period, unix current directory
///    * `..`, double dot, unix parent directory
/// TODO: There is more validation that could be done here. Windows has a broad list of illegal
/// characters and reseved words.
///
/// It should be noted that `RepoPathBuf` and `RepoPath` implement `AsRef<RepoPath>`.
#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct RepoPath(str);

/// An owned version of a `PathComponent`. Not intended for mutation. RepoPathBuf is probably
/// more appropriate for mutation.
#[derive(Clone, Debug, Default, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct PathComponentBuf(String);

/// A `RepoPath` is a series of `PathComponent`s joined together by a separator (`/`).
/// Names for directories or files.
#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct PathComponent(str);

/// The One. The One Character We Use To Separate Paths Into Components.
pub const SEPARATOR: char = '/';

impl RepoPathBuf {
    /// Constructs an empty RepoPathBuf. This path will have no
    /// components and will be equivalent to the root of the repository.
    pub fn new() -> RepoPathBuf {
        Default::default()
    }

    /// Constructs a `RepoPathBuf` from a `String`. It can fail when the contents of String is
    /// deemed invalid. See `RepoPath` for validation rules.
    pub fn from_string(s: String) -> Fallible<Self> {
        validate_path(&s)?;
        Ok(RepoPathBuf(s))
    }

    /// Converts the `RepoPathBuf` in a `RepoPath`.
    pub fn as_repo_path(&self) -> &RepoPath {
        self
    }

    /// Returns whether the current `RepoPathBuf` has no components. Since `RepoPathBuf`
    /// represents the relative path from the start of the repository this is equivalent to
    /// checking whether the path is the root of the repository
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Append a `RepoPath` to the end of `RepoPathBuf`. This function will add the `SEPARATOR`
    /// required by concatenation.
    pub fn push<P: AsRef<RepoPath>>(&mut self, path: P) {
        self.append(&path.as_ref().0);
    }

    /// Removed the last component from the `RepoPathBuf` and return it.
    pub fn pop(&mut self) -> Option<PathComponentBuf> {
        if self.0.is_empty() {
            return None;
        }
        match self.0.rfind(SEPARATOR) {
            None => {
                let result = PathComponentBuf::from_string_unchecked(self.0.clone());
                self.0 = String::new();
                Some(result)
            }
            Some(pos) => {
                let result = PathComponentBuf::from_string_unchecked(self.0.split_off(pos + 1));
                self.0.pop(); // remove SEPARATOR
                Some(result)
            }
        }
    }

    fn append(&mut self, s: &str) {
        if !self.0.is_empty() {
            self.0.push(SEPARATOR);
        }
        self.0.push_str(s);
    }
}

impl Deref for RepoPathBuf {
    type Target = RepoPath;
    fn deref(&self) -> &Self::Target {
        unsafe { mem::transmute(&*self.0) }
    }
}

impl AsRef<RepoPath> for RepoPathBuf {
    fn as_ref(&self) -> &RepoPath {
        self
    }
}

impl Borrow<RepoPath> for RepoPathBuf {
    fn borrow(&self) -> &RepoPath {
        self
    }
}

impl fmt::Display for RepoPathBuf {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&*self.0, formatter)
    }
}

impl RepoPath {
    /// Returns an empty `RepoPath`. Parallel to `RepoPathBuf::new()`. This path will have no
    /// components and will be equivalent to the root of the repository.
    pub fn empty() -> &'static RepoPath {
        RepoPath::from_str_unchecked("")
    }

    /// Returns whether the current `RepoPath` has no components. Since `RepoPath`
    /// represents the relative path from the start of the repository this is equivalent to
    /// checking whether the path is the root of the repository
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Constructs a `RepoPath` from a byte slice. It will fail when the bytes are are not valid
    /// utf8 or when the string does not respect the `RepoPath` rules.
    pub fn from_utf8<'a, S: AsRef<[u8]>>(s: &'a S) -> Fallible<&'a RepoPath> {
        let utf8_str = std::str::from_utf8(s.as_ref())?;
        RepoPath::from_str(utf8_str)
    }

    /// Constructs a `RepoPath` from a `str` slice. It will fail when the string does not respect
    /// the `RepoPath` rules.
    pub fn from_str(s: &str) -> Fallible<&RepoPath> {
        validate_path(s)?;
        Ok(RepoPath::from_str_unchecked(s))
    }

    fn from_str_unchecked(s: &str) -> &RepoPath {
        unsafe { mem::transmute(s) }
    }

    /// Returns the underlying bytes of the `RepoPath`.
    pub fn as_byte_slice(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Return the parent of the path. The empty path, `RepoPath::empty()` does not have a
    /// parent so `None` is returned in that case.
    pub fn parent(&self) -> Option<&RepoPath> {
        self.split_last_component().map(|(parent, _)| parent)
    }

    /// Return the last component of the path. The empty path, `RepoPath::empty()` does not have
    /// any components so `None` is returned in that case.
    pub fn last_component(&self) -> Option<&PathComponent> {
        self.split_last_component().map(|(_, component)| component)
    }

    /// Tries to split the current `RepoPath` in a parent path and a component. If the current
    /// path is empty then None is returned. If the current path contains only one component then
    /// the pair that is returned is the empty repo path and a path component that will match the
    /// contents `self`.
    pub fn split_last_component(&self) -> Option<(&RepoPath, &PathComponent)> {
        if self.is_empty() {
            return None;
        }
        match self.0.rfind(SEPARATOR) {
            Some(pos) => Some((
                RepoPath::from_str_unchecked(&self.0[..pos]),
                PathComponent::from_str_unchecked(&self.0[(pos + 1)..]),
            )),
            None => Some((
                RepoPath::empty(),
                PathComponent::from_str_unchecked(&self.0),
            )),
        }
    }

    /// Returns an iterator over the parents of the current path.
    /// The `RepoPath` itself is not returned. The root of the repository represented by the empty
    /// `RepoPath` is always returned by this iterator except if the path is empty.
    ///
    /// For example for the path `"foo/bar/baz"` this iterator will return three items:
    /// `""`, `"foo"` and `"foo/bar"`.
    ///
    /// If you don't want to handle the empty path, then you can use `parents().skip(1)`.
    /// It is possible to get iterate over parents with elements in paralel using:
    /// `path.parents().zip(path.components())`.
    pub fn parents<'a>(&'a self) -> Parents<'a> {
        Parents::new(self)
    }

    /// Returns an iterator over the components of the path.
    pub fn components<'a>(&'a self) -> Components<'a> {
        Components::new(self)
    }
}

impl AsRef<RepoPath> for RepoPath {
    fn as_ref(&self) -> &RepoPath {
        self
    }
}

impl AsRef<[u8]> for RepoPath {
    fn as_ref(&self) -> &[u8] {
        self.as_byte_slice()
    }
}

impl ToOwned for RepoPath {
    type Owned = RepoPathBuf;
    fn to_owned(&self) -> Self::Owned {
        RepoPathBuf(self.0.to_string())
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl PathComponentBuf {
    /// Constructs an from a `String`. It can fail when the contents of `String` is deemed invalid.
    /// See `PathComponent` for validation rules.
    pub fn from_string(s: String) -> Fallible<Self> {
        validate_component(&s)?;
        Ok(PathComponentBuf(s))
    }

    /// Converts the `PathComponentBuf` in a `PathComponent`.
    pub fn as_path_component(&self) -> &PathComponent {
        self
    }

    fn from_string_unchecked(s: String) -> Self {
        PathComponentBuf(s)
    }
}

impl Deref for PathComponentBuf {
    type Target = PathComponent;
    fn deref(&self) -> &Self::Target {
        unsafe { mem::transmute(&*self.0) }
    }
}

impl AsRef<PathComponent> for PathComponentBuf {
    fn as_ref(&self) -> &PathComponent {
        self
    }
}

impl Borrow<PathComponent> for PathComponentBuf {
    fn borrow(&self) -> &PathComponent {
        self
    }
}

impl fmt::Display for PathComponentBuf {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&*self.0, formatter)
    }
}

impl PathComponent {
    /// Constructs a `PathComponent` from a byte slice. It will fail when the bytes are are not
    /// valid utf8 or when the string does not respect the `PathComponent` rules.
    pub fn from_utf8(s: &[u8]) -> Fallible<&PathComponent> {
        let utf8_str = std::str::from_utf8(s)?;
        PathComponent::from_str(utf8_str)
    }

    /// Constructs a `PathComponent` from a `str` slice. It will fail when the string does not
    /// respect the `PathComponent` rules.
    pub fn from_str(s: &str) -> Fallible<&PathComponent> {
        validate_component(s)?;
        Ok(PathComponent::from_str_unchecked(s))
    }

    fn from_str_unchecked(s: &str) -> &PathComponent {
        unsafe { mem::transmute(s) }
    }

    /// Returns the underlying bytes of the `PathComponent`.
    pub fn as_byte_slice(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl AsRef<PathComponent> for PathComponent {
    fn as_ref(&self) -> &PathComponent {
        self
    }
}

impl AsRef<RepoPath> for PathComponent {
    fn as_ref(&self) -> &RepoPath {
        unsafe { mem::transmute(&self.0) }
    }
}

impl AsRef<[u8]> for PathComponent {
    fn as_ref(&self) -> &[u8] {
        self.as_byte_slice()
    }
}

impl ToOwned for PathComponent {
    type Owned = PathComponentBuf;
    fn to_owned(&self) -> Self::Owned {
        PathComponentBuf(self.0.to_string())
    }
}

impl fmt::Display for PathComponent {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

fn validate_path(s: &str) -> Fallible<()> {
    if s.is_empty() {
        return Ok(());
    }
    if s.bytes().next_back() == Some(b'/') {
        bail!("Invalid path: ends with `/`.");
    }
    for component in s.split(SEPARATOR) {
        validate_component(component)?;
    }
    Ok(())
}

fn validate_component(s: &str) -> Fallible<()> {
    if s.is_empty() {
        bail!("Invalid component: empty.");
    }
    if s == "." || s == ".." {
        bail!("Invalid component: {}", s);
    }
    for b in s.bytes() {
        if b == 0u8 || b == 1u8 || b == b'\n' || b == b'/' {
            bail!("Invalid component: contains byte {}.", b);
        }
    }
    Ok(())
}

pub struct Parents<'a> {
    path: &'a RepoPath,
    position: Option<usize>,
}

impl<'a> Parents<'a> {
    pub fn new(path: &'a RepoPath) -> Self {
        Parents {
            path,
            position: None,
        }
    }
}

impl<'a> Iterator for Parents<'a> {
    type Item = &'a RepoPath;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(ref mut position) = self.position {
            match self.path.0[*position..].find(SEPARATOR) {
                Some(delta) => {
                    let end = *position + delta;
                    let result = RepoPath::from_str_unchecked(&self.path.0[..end]);
                    *position = end + 1;
                    Some(result)
                }
                None => {
                    *position = self.path.0.len();
                    None
                }
            }
        } else {
            self.position = Some(0);
            if self.path.is_empty() {
                None
            } else {
                Some(RepoPath::empty())
            }
        }
    }
}

pub struct Components<'a> {
    path: &'a RepoPath,
    position: usize,
}

impl<'a> Components<'a> {
    pub fn new(path: &'a RepoPath) -> Self {
        Components { path, position: 0 }
    }
}

impl<'a> Iterator for Components<'a> {
    type Item = &'a PathComponent;

    fn next(&mut self) -> Option<Self::Item> {
        match self.path.0[self.position..].find(SEPARATOR) {
            Some(delta) => {
                let end = self.position + delta;
                let result = PathComponent::from_str_unchecked(&self.path.0[self.position..end]);
                self.position = end + 1;
                Some(result)
            }
            None => {
                if self.position < self.path.0.len() {
                    let result = PathComponent::from_str_unchecked(&self.path.0[self.position..]);
                    self.position = self.path.0.len();
                    Some(result)
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl quickcheck::Arbitrary for RepoPathBuf {
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        let size = g.gen_range(0, 16);
        let mut path_buf = RepoPathBuf::new();
        for _ in 0..size {
            path_buf.push(PathComponentBuf::arbitrary(g).as_ref());
        }
        path_buf
    }
}

#[cfg(any(test, feature = "for-tests"))]
impl quickcheck::Arbitrary for PathComponentBuf {
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        // Most strings should be valid `PathComponent` so it is reasonable to loop until a valid
        // string is found. To note that generating Arbitrary Unicode on `char` is implemented
        // using a loop where random bytes are validated against the `char` constructor.
        loop {
            let s = String::arbitrary(g);
            if let Ok(component) = PathComponentBuf::from_string(s) {
                return component;
            }
        }
    }

    fn shrink(&self) -> Box<Iterator<Item = PathComponentBuf>> {
        Box::new(
            self.0
                .shrink()
                .filter_map(|s| PathComponentBuf::from_string(s).ok()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck::quickcheck;

    #[test]
    fn test_repo_path_initialization_with_invalid_utf8() {
        assert!(RepoPath::from_utf8(&vec![0x80, 0x80]).is_err());
    }

    #[test]
    fn test_path_display() {
        assert_eq!(
            format!("{}", RepoPath::from_utf8(b"slice").unwrap()),
            "slice"
        );
        assert_eq!(format!("{}", RepoPath::from_str("slice").unwrap()), "slice");
    }

    #[test]
    fn test_path_debug() {
        assert_eq!(
            format!("{:?}", RepoPath::from_utf8(b"slice").unwrap()),
            "RepoPath(\"slice\")"
        );
        assert_eq!(
            format!("{:?}", RepoPath::from_str("slice").unwrap()),
            "RepoPath(\"slice\")"
        );
    }

    #[test]
    fn test_pathbuf_display() {
        assert_eq!(format!("{}", RepoPathBuf::new()), "");
        assert_eq!(
            format!(
                "{}",
                RepoPathBuf::from_string(String::from("slice")).unwrap()
            ),
            "slice"
        );
    }

    #[test]
    fn test_pathbuf_debug() {
        assert_eq!(format!("{:?}", RepoPathBuf::new()), "RepoPathBuf(\"\")");
        assert_eq!(
            format!(
                "{:?}",
                RepoPathBuf::from_string(String::from("slice")).unwrap()
            ),
            "RepoPathBuf(\"slice\")"
        );
    }

    #[test]
    fn test_repo_path_conversions() {
        let repo_path_buf = RepoPathBuf::from_string(String::from("path_buf")).unwrap();
        assert_eq!(repo_path_buf.as_ref().to_owned(), repo_path_buf);

        let repo_path = RepoPath::from_str("path").unwrap();
        assert_eq!(repo_path.to_owned().as_ref(), repo_path);
    }

    #[test]
    fn test_repo_path_buf_push() {
        let mut repo_path_buf = RepoPathBuf::new();
        repo_path_buf.push(RepoPath::from_str("one").unwrap());
        assert_eq!(repo_path_buf.as_ref(), RepoPath::from_str("one").unwrap());
        repo_path_buf.push(RepoPath::from_str("two").unwrap());
        assert_eq!(
            repo_path_buf.as_ref(),
            RepoPath::from_str("one/two").unwrap()
        );
    }

    #[test]
    fn test_repo_path_buf_pop() {
        let mut repo_path_buf = RepoPathBuf::from_string(String::from("one/two/three")).unwrap();
        assert_eq!(
            repo_path_buf.pop(),
            Some(PathComponentBuf::from_string(String::from("three")).unwrap())
        );
        assert_eq!(
            repo_path_buf,
            RepoPathBuf::from_string(String::from("one/two")).unwrap()
        );
        assert_eq!(
            repo_path_buf.pop(),
            Some(PathComponentBuf::from_string(String::from("two")).unwrap())
        );
        assert_eq!(
            repo_path_buf,
            RepoPathBuf::from_string(String::from("one")).unwrap()
        );
        assert_eq!(
            repo_path_buf.pop(),
            Some(PathComponentBuf::from_string(String::from("one")).unwrap())
        );
        assert_eq!(repo_path_buf, RepoPathBuf::new());
        assert_eq!(repo_path_buf.pop(), None);
    }

    #[test]
    fn test_component_initialization_with_invalid_utf8() {
        assert!(PathComponent::from_utf8(&vec![0x80, 0x80]).is_err());
    }

    #[test]
    fn test_component_display() {
        assert_eq!(
            format!("{}", PathComponent::from_utf8(b"slice").unwrap()),
            "slice"
        );
        assert_eq!(
            format!("{}", PathComponent::from_str("slice").unwrap()),
            "slice"
        );
    }

    #[test]
    fn test_component_debug() {
        assert_eq!(
            format!("{:?}", PathComponent::from_utf8(b"slice").unwrap()),
            "PathComponent(\"slice\")"
        );
        assert_eq!(
            format!("{:?}", PathComponent::from_str("slice").unwrap()),
            "PathComponent(\"slice\")"
        )
    }

    #[test]
    fn test_componentbuf_display() {
        assert_eq!(
            format!(
                "{}",
                PathComponentBuf::from_string(String::from("slice")).unwrap()
            ),
            "slice",
        );
    }

    #[test]
    fn test_componentbuf_debug() {
        assert_eq!(
            format!(
                "{:?}",
                PathComponentBuf::from_string(String::from("slice")).unwrap()
            ),
            "PathComponentBuf(\"slice\")"
        );
    }

    #[test]
    fn test_component_conversions() {
        let componentbuf = PathComponentBuf::from_string(String::from("componentbuf")).unwrap();
        assert_eq!(componentbuf.as_ref().to_owned(), componentbuf);

        let component = PathComponent::from_str("component").unwrap();
        assert_eq!(component.to_owned().as_ref(), component);
    }

    #[test]
    fn test_path_components() {
        let mut iter = RepoPath::from_str("foo/bar/baz.txt").unwrap().components();
        assert_eq!(
            iter.next().unwrap(),
            PathComponent::from_str("foo").unwrap()
        );
        assert_eq!(
            iter.next().unwrap(),
            PathComponent::from_str("bar").unwrap()
        );
        assert_eq!(
            iter.next().unwrap(),
            PathComponent::from_str("baz.txt").unwrap()
        );
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_append_component_to_path() {
        let expected = RepoPath::from_str("foo/bar/baz.txt").unwrap();
        let mut pathbuf = RepoPathBuf::new();
        for component in expected.components() {
            pathbuf.push(component);
        }
        assert_eq!(pathbuf.deref(), expected);
    }

    #[test]
    fn test_validate_path() {
        assert_eq!(
            format!("{}", validate_path("\n").unwrap_err()),
            "Invalid component: contains byte 10."
        );
        assert_eq!(
            format!("{}", validate_path("boo/").unwrap_err()),
            "Invalid path: ends with `/`."
        );
    }

    #[test]
    fn test_validate_component() {
        assert_eq!(
            format!("{}", validate_component("foo/bar").unwrap_err()),
            "Invalid component: contains byte 47."
        );
        assert_eq!(
            format!("{}", validate_component("").unwrap_err()),
            "Invalid component: empty."
        );
    }

    #[test]
    fn test_empty_path_components() {
        assert_eq!(RepoPathBuf::new().components().next(), None);
        assert_eq!(RepoPath::empty().components().next(), None);
    }

    #[test]
    fn test_empty_path_is_empty() {
        assert!(RepoPathBuf::new().is_empty());
        assert!(RepoPath::empty().is_empty());
    }

    #[test]
    fn test_parent() {
        assert_eq!(RepoPath::empty().parent(), None);
        assert_eq!(
            RepoPath::from_str("foo").unwrap().parent(),
            Some(RepoPath::empty())
        );
        assert_eq!(
            RepoPath::from_str("foo/bar/baz").unwrap().parent(),
            Some(RepoPath::from_str("foo/bar").unwrap())
        );
    }

    #[test]
    fn test_last_component() {
        assert_eq!(RepoPath::empty().last_component(), None);
        assert_eq!(
            RepoPath::from_str("foo").unwrap().last_component(),
            Some(PathComponent::from_str("foo").unwrap())
        );
        assert_eq!(
            RepoPath::from_str("foo/bar/baz").unwrap().last_component(),
            Some(PathComponent::from_str("baz").unwrap())
        );
    }

    #[test]
    fn test_parents_on_regular_path() {
        let path = RepoPath::from_str("foo/bar/baz/file.txt").unwrap();
        let mut iter = path.parents();
        assert_eq!(iter.next(), Some(RepoPath::empty()));
        assert_eq!(iter.next(), Some(RepoPath::from_str("foo").unwrap()));
        assert_eq!(iter.next(), Some(RepoPath::from_str("foo/bar").unwrap()));
        assert_eq!(
            iter.next(),
            Some(RepoPath::from_str("foo/bar/baz").unwrap())
        );
        assert_eq!(iter.next(), None)
    }

    #[test]
    fn test_parents_on_empty_path() {
        assert_eq!(RepoPath::empty().parents().next(), None);
    }

    #[test]
    fn test_parents_and_components_in_parallel() {
        let path = RepoPath::from_str("foo/bar/baz").unwrap();
        let mut iter = path.parents().zip(path.components());
        assert_eq!(
            iter.next(),
            Some((RepoPath::empty(), PathComponent::from_str("foo").unwrap()))
        );
        assert_eq!(
            iter.next(),
            Some((
                RepoPath::from_str("foo").unwrap(),
                PathComponent::from_str("bar").unwrap()
            ))
        );
        assert_eq!(
            iter.next(),
            Some((
                RepoPath::from_str("foo/bar").unwrap(),
                PathComponent::from_str("baz").unwrap()
            ))
        );
        assert_eq!(iter.next(), None);
    }

    quickcheck! {
       fn test_parents_equal_components(path: RepoPathBuf) -> bool {
           path.deref().parents().count() == path.deref().components().count()
        }
    }

    #[test]
    fn test_split_last_component() {
        assert_eq!(RepoPath::empty().split_last_component(), None);

        assert_eq!(
            RepoPath::from_str("foo").unwrap().split_last_component(),
            Some((RepoPath::empty(), PathComponent::from_str("foo").unwrap()))
        );

        assert_eq!(
            RepoPath::from_str("foo/bar/baz")
                .unwrap()
                .split_last_component(),
            Some((
                RepoPath::from_str("foo/bar").unwrap(),
                PathComponent::from_str("baz").unwrap()
            ))
        );
    }

    #[test]
    fn test_to_owned() {
        assert_eq!(RepoPath::empty().to_owned(), RepoPathBuf::new());
        assert_eq!(
            RepoPath::from_str("foo/bar").unwrap().to_owned(),
            RepoPathBuf::from_string(String::from("foo/bar")).unwrap()
        );
        assert_eq!(
            PathComponent::from_str("foo").unwrap().to_owned(),
            PathComponentBuf::from_string(String::from("foo")).unwrap()
        );
    }
}
