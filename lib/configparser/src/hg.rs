// Copyright 2018 Facebook, Inc.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Mercurial-specific config postprocessing

use std::cmp::Eq;
use std::collections::{HashMap, HashSet};
use std::env;
use std::hash::Hash;
use std::path::Path;

use bytes::Bytes;
use dirs;

use crate::config::{expand_path, ConfigSet, Options};
use crate::error::Error;

const HGPLAIN: &str = "HGPLAIN";
const HGPLAINEXCEPT: &str = "HGPLAINEXCEPT";
const HGRCPATH: &str = "HGRCPATH";

pub trait OptionsHgExt {
    /// Drop configs according to `$HGPLAIN` and `$HGPLAINEXCEPT`.
    fn process_hgplain(self) -> Self;

    /// Set read-only config items. `items` contains a list of tuple `(section, name)`.
    /// Setting those items to new value will be ignored.
    fn readonly_items<S: Into<Bytes>, N: Into<Bytes>>(self, items: Vec<(S, N)>) -> Self;

    /// Set section remap. If a section name matches an entry key, it will be treated as if the
    /// name is the entry value. The remap wouldn't happen recursively. For example, with a
    /// `{"A": "B", "B": "C"}` map, section name "A" will be treated as "B", not "C".
    /// This is implemented via `append_filter`.
    fn remap_sections<K: Eq + Hash + Into<Bytes>, V: Into<Bytes>>(
        self,
        remap: HashMap<K, V>,
    ) -> Self;

    /// Set section whitelist. Sections outside the whitelist won't be loaded.
    /// This is implemented via `append_filter`.
    fn whitelist_sections<B: Clone + Into<Bytes>>(self, sections: Vec<B>) -> Self;
}

pub trait ConfigSetHgExt {
    /// Load system config files if `$HGRCPATH` is not set.
    /// `data_dir` is `mercurial.util.datapath`.
    /// Return errors parsing files.
    fn load_system<P: AsRef<Path>>(&mut self, data_dir: P) -> Vec<Error>;

    /// Load user config files (and environment variables).  If `$HGRCPATH` is
    /// set, load files listed in that environment variable instead.
    /// Return errors parsing files.
    fn load_user(&mut self) -> Vec<Error>;
}

impl OptionsHgExt for Options {
    fn process_hgplain(self) -> Self {
        let plain_set = env::var(HGPLAIN).is_ok();
        let plain_except = env::var(HGPLAINEXCEPT);
        if plain_set || plain_except.is_ok() {
            let (section_blacklist, ui_blacklist) = {
                let plain_exceptions: HashSet<String> = plain_except
                    .unwrap_or_else(|_| "".to_string())
                    .split(',')
                    .map(|s| s.to_string())
                    .collect();

                // [defaults] and [commands] are always blacklisted.
                let mut section_blacklist: HashSet<Bytes> =
                    ["defaults", "commands"].iter().map(|&s| s.into()).collect();

                // [alias], [revsetalias], [templatealias] are blacklisted if they are outside
                // HGPLAINEXCEPT.
                for &name in ["alias", "revsetalias", "templatealias"].iter() {
                    if !plain_exceptions.contains(name) {
                        section_blacklist.insert(Bytes::from(name));
                    }
                }

                // These configs under [ui] are always blacklisted.
                let mut ui_blacklist: HashSet<Bytes> = [
                    "debug",
                    "fallbackencoding",
                    "quiet",
                    "slash",
                    "logtemplate",
                    "statuscopies",
                    "style",
                    "traceback",
                    "verbose",
                ]
                .iter()
                .map(|&s| s.into())
                .collect();
                // exitcodemask is blacklisted if exitcode is outside HGPLAINEXCEPT.
                if !plain_exceptions.contains("exitcode") {
                    ui_blacklist.insert("exitcodemask".into());
                }

                (section_blacklist, ui_blacklist)
            };

            let filter = move |section: Bytes, name: Bytes, value: Option<Bytes>| {
                if section_blacklist.contains(&section)
                    || (section.as_ref() == b"ui" && ui_blacklist.contains(&name))
                {
                    None
                } else {
                    Some((section, name, value))
                }
            };

            self.append_filter(Box::new(filter))
        } else {
            self
        }
    }

    /// Set section whitelist. Sections outside the whitelist won't be loaded.
    /// This is implemented via `append_filter`.
    fn whitelist_sections<B: Clone + Into<Bytes>>(self, sections: Vec<B>) -> Self {
        let whitelist: HashSet<Bytes> = sections
            .iter()
            .cloned()
            .map(|section| section.into())
            .collect();

        let filter = move |section: Bytes, name: Bytes, value: Option<Bytes>| {
            if whitelist.contains(&section) {
                Some((section, name, value))
            } else {
                None
            }
        };

        self.append_filter(Box::new(filter))
    }

    /// Set section remap. If a section name matches an entry key, it will be treated as if the
    /// name is the entry value. The remap wouldn't happen recursively. For example, with a
    /// `{"A": "B", "B": "C"}` map, section name "A" will be treated as "B", not "C".
    /// This is implemented via `append_filter`.
    fn remap_sections<K, V>(self, remap: HashMap<K, V>) -> Self
    where
        K: Eq + Hash + Into<Bytes>,
        V: Into<Bytes>,
    {
        let remap: HashMap<Bytes, Bytes> = remap
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();

        let filter = move |section: Bytes, name: Bytes, value: Option<Bytes>| {
            let section = remap.get(&section).cloned().unwrap_or(section);
            Some((section, name, value))
        };

        self.append_filter(Box::new(filter))
    }

    fn readonly_items<S: Into<Bytes>, N: Into<Bytes>>(self, items: Vec<(S, N)>) -> Self {
        let readonly_items: HashSet<(Bytes, Bytes)> = items
            .into_iter()
            .map(|(section, name)| (section.into(), name.into()))
            .collect();

        let filter = move |section: Bytes, name: Bytes, value: Option<Bytes>| {
            if readonly_items.contains(&(section.clone(), name.clone())) {
                None
            } else {
                Some((section, name, value))
            }
        };

        self.append_filter(Box::new(filter))
    }
}

impl ConfigSetHgExt for ConfigSet {
    fn load_system<P: AsRef<Path>>(&mut self, data_dir: P) -> Vec<Error> {
        let opts = Options::new().source("system").process_hgplain();
        let data_dir = data_dir.as_ref();
        let mut errors = Vec::new();

        if env::var(HGRCPATH).is_err() {
            #[cfg(unix)]
            {
                errors.append(&mut self.load_path(data_dir.join("default.d/mergetools.rc"), &opts));
                errors.append(&mut self.load_path("/etc/mercurial/system.rc", &opts));
                // TODO(T40519286): Remove this after the tupperware overrides move out of hgrc.d
                errors.append(
                    &mut self.load_path("/etc/mercurial/hgrc.d/tupperware_overrides.rc", &opts),
                );
                // TODO(quark): Remove this after packages using system.rc are rolled out
                errors.append(&mut self.load_path("/etc/mercurial/hgrc.d/include.rc", &opts));
            }

            #[cfg(windows)]
            {
                errors.append(&mut self.load_path(data_dir.join("default.d/mergetools.rc"), &opts));
                if let Ok(program_data_path) = env::var("PROGRAMDATA") {
                    let hgrc_dir = Path::new(&program_data_path).join("Facebook\\Mercurial");
                    errors.append(&mut self.load_path(hgrc_dir.join("system.rc"), &opts));
                    // TODO(quark): Remove this after packages using system.rc are rolled out
                    errors.append(&mut self.load_path(hgrc_dir.join("hgrc"), &opts));
                }
            }
        }

        errors
    }

    fn load_user(&mut self) -> Vec<Error> {
        let mut errors = Vec::new();

        // Covert "$VISUAL", "$EDITOR" to "ui.editor".
        //
        // Unlike Mercurial, don't convert the "$PAGER" environment variable
        // to "pager.pager" config.
        //
        // The environment variable could be from the system profile (ex.
        // /etc/profile.d/...), or the user shell rc (ex. ~/.bashrc). There is
        // no clean way to tell which one it is from.  The value might be
        // tweaked for sysadmin usecases (ex. -n), which are different from
        // SCM's usecases.
        for name in ["VISUAL", "EDITOR"].iter() {
            if let Ok(editor) = env::var(name) {
                self.set(
                    "ui",
                    "editor",
                    Some(editor.as_bytes()),
                    &Options::new().source(format!("${}", name)),
                );
                break;
            }
        }

        // Convert $HGPROF to profiling.type
        if let Ok(profiling_type) = env::var("HGPROF") {
            self.set(
                "profiling",
                "type",
                Some(profiling_type.as_bytes()),
                &"$HGPROF".into(),
            );
        }

        let opts = Options::new().source("user").process_hgplain();

        // If $HGRCPATH is set, use it instead.
        if let Ok(rcpath) = env::var("HGRCPATH") {
            #[cfg(unix)]
            let paths = rcpath.split(':');
            #[cfg(windows)]
            let paths = rcpath.split(';');
            for path in paths {
                errors.append(&mut self.load_path(expand_path(path), &opts));
            }
        } else {
            if let Some(home_dir) = dirs::home_dir() {
                errors.append(&mut self.load_path(home_dir.join(".hgrc"), &opts));

                #[cfg(windows)]
                {
                    errors.append(&mut self.load_path(home_dir.join("mercurial.ini"), &opts));
                }
            }
            if let Some(config_dir) = dirs::config_dir() {
                errors.append(&mut self.load_path(config_dir.join("hg/hgrc"), &opts));
            }
        }

        errors
    }
}

/// Parse a configuration value as a list of comma/space separated strings.
/// It is ported from `mercurial.config.parselist`.
///
/// The function never complains about syntax and always returns some result.
///
/// Example:
///
/// ```
/// use configparser::hg::parse_list;
///
/// assert_eq!(
///     parse_list(b"this,is \"a small\" ,test"),
///     vec![b"this".to_vec(), b"is".to_vec(), b"a small".to_vec(), b"test".to_vec()]
/// );
/// ```
pub fn parse_list<B: AsRef<[u8]>>(value: B) -> Vec<Bytes> {
    let mut value = value.as_ref();

    // ```python
    // if value is not None and isinstance(value, bytes):
    //     result = _configlist(value.lstrip(' ,\n'))
    // ```

    while b" ,\n".iter().any(|b| value.starts_with(&[*b])) {
        value = &value[1..]
    }

    parse_list_internal(value)
        .into_iter()
        .map(Bytes::from)
        .collect()
}

fn parse_list_internal(value: &[u8]) -> Vec<Vec<u8>> {
    let mut value = value;

    // ```python
    // def _configlist(s):
    //     s = s.rstrip(' ,')
    //     if not s:
    //         return []
    //     parser, parts, offset = _parse_plain, [''], 0
    //     while parser:
    //         parser, parts, offset = parser(parts, s, offset)
    //     return parts
    // ```

    while b" ,\n".iter().any(|b| value.ends_with(&[*b])) {
        value = &value[..value.len() - 1]
    }

    if value.is_empty() {
        return Vec::new();
    }

    #[derive(Copy, Clone)]
    enum State {
        Plain,
        Quote,
    };

    let mut offset = 0;
    let mut parts: Vec<Vec<u8>> = vec![Vec::new()];
    let mut state = State::Plain;

    loop {
        match state {
            // ```python
            // def _parse_plain(parts, s, offset):
            //     whitespace = False
            //     while offset < len(s) and (s[offset:offset + 1].isspace()
            //                                or s[offset:offset + 1] == ','):
            //         whitespace = True
            //         offset += 1
            //     if offset >= len(s):
            //         return None, parts, offset
            //     if whitespace:
            //         parts.append('')
            //     if s[offset:offset + 1] == '"' and not parts[-1]:
            //         return _parse_quote, parts, offset + 1
            //     elif s[offset:offset + 1] == '"' and parts[-1][-1:] == '\\':
            //         parts[-1] = parts[-1][:-1] + s[offset:offset + 1]
            //         return _parse_plain, parts, offset + 1
            //     parts[-1] += s[offset:offset + 1]
            //     return _parse_plain, parts, offset + 1
            // ```
            State::Plain => {
                let mut whitespace = false;
                while offset < value.len() && b" \n\r\t,".contains(&value[offset]) {
                    whitespace = true;
                    offset += 1;
                }
                if offset >= value.len() {
                    break;
                }
                if whitespace {
                    parts.push(Vec::new());
                }
                if value[offset] == b'"' {
                    let branch = {
                        match parts.last() {
                            None => 1,
                            Some(last) => {
                                if last.is_empty() {
                                    1
                                } else if last.ends_with(b"\\") {
                                    2
                                } else {
                                    3
                                }
                            }
                        }
                    }; // manual NLL, to drop reference on "parts".
                    if branch == 1 {
                        // last.is_empty()
                        state = State::Quote;
                        offset += 1;
                        continue;
                    } else if branch == 2 {
                        // last.ends_with(b"\\")
                        let last = parts.last_mut().unwrap();
                        last.pop();
                        last.push(value[offset]);
                        offset += 1;
                        continue;
                    }
                }
                let last = parts.last_mut().unwrap();
                last.push(value[offset]);
                offset += 1;
            }

            // ```python
            // def _parse_quote(parts, s, offset):
            //     if offset < len(s) and s[offset:offset + 1] == '"': # ""
            //         parts.append('')
            //         offset += 1
            //         while offset < len(s) and (s[offset:offset + 1].isspace() or
            //                 s[offset:offset + 1] == ','):
            //             offset += 1
            //         return _parse_plain, parts, offset
            //     while offset < len(s) and s[offset:offset + 1] != '"':
            //         if (s[offset:offset + 1] == '\\' and offset + 1 < len(s)
            //                 and s[offset + 1:offset + 2] == '"'):
            //             offset += 1
            //             parts[-1] += '"'
            //         else:
            //             parts[-1] += s[offset:offset + 1]
            //         offset += 1
            //     if offset >= len(s):
            //         real_parts = _configlist(parts[-1])
            //         if not real_parts:
            //             parts[-1] = '"'
            //         else:
            //             real_parts[0] = '"' + real_parts[0]
            //             parts = parts[:-1]
            //             parts.extend(real_parts)
            //         return None, parts, offset
            //     offset += 1
            //     while offset < len(s) and s[offset:offset + 1] in [' ', ',']:
            //         offset += 1
            //     if offset < len(s):
            //         if offset + 1 == len(s) and s[offset:offset + 1] == '"':
            //             parts[-1] += '"'
            //             offset += 1
            //         else:
            //             parts.append('')
            //     else:
            //         return None, parts, offset
            //     return _parse_plain, parts, offset
            // ```
            State::Quote => {
                if offset < value.len() && value[offset] == b'"' {
                    parts.push(Vec::new());
                    offset += 1;
                    while offset < value.len() && b" \n\r\t,".contains(&value[offset]) {
                        offset += 1;
                    }
                    state = State::Plain;
                    continue;
                }
                while offset < value.len() && value[offset] != b'"' {
                    if value[offset] == b'\\'
                        && offset + 1 < value.len()
                        && value[offset + 1] == b'"'
                    {
                        offset += 1;
                        parts.last_mut().unwrap().push(b'"');
                    } else {
                        parts.last_mut().unwrap().push(value[offset]);
                    }
                    offset += 1;
                }
                if offset >= value.len() {
                    let mut real_parts: Vec<Vec<u8>> = parse_list_internal(parts.last().unwrap())
                        .iter()
                        .map(|b| b.to_vec())
                        .collect();
                    if real_parts.is_empty() {
                        parts.pop();
                        parts.push(vec![b'"']);
                    } else {
                        real_parts[0].insert(0, b'"');
                        parts.pop();
                        parts.append(&mut real_parts);
                    }
                    break;
                }
                offset += 1;
                while offset < value.len() && b" ,".contains(&value[offset]) {
                    offset += 1;
                }
                if offset < value.len() {
                    if offset + 1 == value.len() && value[offset] == b'"' {
                        parts.last_mut().unwrap().push(b'"');
                        offset += 1;
                    } else {
                        parts.push(Vec::new());
                    }
                } else {
                    break;
                }
                state = State::Plain;
            }
        }
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempdir::TempDir;

    use crate::config::tests::write_file;

    #[test]
    fn test_basic_hgplain() {
        env::set_var(HGPLAIN, "1");
        env::remove_var(HGPLAINEXCEPT);

        let opts = Options::new().process_hgplain();
        let mut cfg = ConfigSet::new();
        cfg.parse(
            "[defaults]\n\
             commit = commit -d 0\n\
             [ui]\n\
             verbose = true\n\
             username = test\n\
             [alias]\n\
             l = log\n",
            &opts,
        );

        assert!(cfg.keys("defaults").is_empty());
        assert_eq!(cfg.get("ui", "verbose"), None);
        assert_eq!(cfg.get("ui", "username"), Some("test".into()));
        assert_eq!(cfg.get("alias", "l"), None);
    }

    #[test]
    fn test_hgplainexcept() {
        env::remove_var(HGPLAIN);
        env::set_var(HGPLAINEXCEPT, "alias,revsetalias");

        let opts = Options::new().process_hgplain();
        let mut cfg = ConfigSet::new();
        cfg.parse(
            "[defaults]\n\
             commit = commit -d 0\n\
             [alias]\n\
             l = log\n\
             [templatealias]\n\
             u = user\n\
             [revsetalias]\n\
             @ = master\n",
            &opts,
        );

        assert!(cfg.keys("defaults").is_empty());
        assert_eq!(cfg.get("alias", "l"), Some("log".into()));
        assert_eq!(cfg.get("revsetalias", "@"), Some("master".into()));
        assert_eq!(cfg.get("templatealias", "u"), None);
    }

    #[test]
    fn test_hgrcpath() {
        let dir = TempDir::new("test_hgrcpath").unwrap();

        write_file(dir.path().join("1.rc"), "[x]\na=1");
        write_file(dir.path().join("2.rc"), "[y]\nb=2");

        #[cfg(unix)]
        let hgrcpath = "$T/1.rc:$T/2.rc";
        #[cfg(windows)]
        let hgrcpath = "$T/1.rc;%T%/2.rc";

        env::set_var("T", dir.path());
        env::set_var(HGRCPATH, hgrcpath);

        let mut cfg = ConfigSet::new();

        cfg.load_system("");
        assert!(cfg.sections().is_empty());

        cfg.load_user();
        assert_eq!(cfg.get("x", "a"), Some("1".into()));
        assert_eq!(cfg.get("y", "b"), Some("2".into()));
    }

    #[test]
    fn test_section_whitelist() {
        let opts = Options::new().whitelist_sections(vec!["x", "y"]);
        let mut cfg = ConfigSet::new();
        cfg.parse(
            "[x]\n\
             a=1\n\
             [y]\n\
             b=2\n\
             [z]\n\
             c=3",
            &opts,
        );

        assert_eq!(cfg.sections(), vec![Bytes::from("x"), Bytes::from("y")]);
        assert_eq!(cfg.get("z", "c"), None);
    }

    #[test]
    fn test_section_remap() {
        let mut remap = HashMap::new();
        remap.insert("x", "y");
        remap.insert("y", "z");

        let opts = Options::new().remap_sections(remap);
        let mut cfg = ConfigSet::new();
        cfg.parse(
            "[x]\n\
             a=1\n\
             [y]\n\
             b=2\n\
             [z]\n\
             c=3",
            &opts,
        );

        assert_eq!(cfg.get("y", "a"), Some("1".into()));
        assert_eq!(cfg.get("z", "b"), Some("2".into()));
        assert_eq!(cfg.get("z", "c"), Some("3".into()));
    }

    #[test]
    fn test_readonly_items() {
        let opts = Options::new().readonly_items(vec![("x", "a"), ("y", "b")]);
        let mut cfg = ConfigSet::new();
        cfg.parse(
            "[x]\n\
             a=1\n\
             [y]\n\
             b=2\n\
             [z]\n\
             c=3",
            &opts,
        );

        assert_eq!(cfg.get("x", "a"), None);
        assert_eq!(cfg.get("y", "b"), None);
        assert_eq!(cfg.get("z", "c"), Some("3".into()));
    }

    #[test]
    fn test_parse_list() {
        fn b<B: AsRef<[u8]>>(bytes: B) -> Bytes {
            Bytes::from(bytes.as_ref())
        }

        // From test-ui-config.py
        assert_eq!(parse_list(b"foo"), vec![b("foo")]);
        assert_eq!(
            parse_list(b"foo bar baz"),
            vec![b("foo"), b("bar"), b("baz")]
        );
        assert_eq!(parse_list(b"alice, bob"), vec![b("alice"), b("bob")]);
        assert_eq!(
            parse_list(b"foo bar baz alice, bob"),
            vec![b("foo"), b("bar"), b("baz"), b("alice"), b("bob")]
        );
        assert_eq!(
            parse_list(b"abc d\"ef\"g \"hij def\""),
            vec![b("abc"), b("d\"ef\"g"), b("hij def")]
        );
        assert_eq!(
            parse_list(b"\"hello world\", \"how are you?\""),
            vec![b("hello world"), b("how are you?")]
        );
        assert_eq!(
            parse_list(b"Do\"Not\"Separate"),
            vec![b("Do\"Not\"Separate")]
        );
        assert_eq!(parse_list(b"\"Do\"Separate"), vec![b("Do"), b("Separate")]);
        assert_eq!(
            parse_list(b"\"Do\\\"NotSeparate\""),
            vec![b("Do\"NotSeparate")]
        );
        assert_eq!(
            parse_list(&b"string \"with extraneous\" quotation mark\""[..]),
            vec![
                b("string"),
                b("with extraneous"),
                b("quotation"),
                b("mark\""),
            ]
        );
        assert_eq!(parse_list(b"x, y"), vec![b("x"), b("y")]);
        assert_eq!(parse_list(b"\"x\", \"y\""), vec![b("x"), b("y")]);
        assert_eq!(
            parse_list(b"\"\"\" key = \"x\", \"y\" \"\"\""),
            vec![b(""), b(" key = "), b("x\""), b("y"), b(""), b("\"")]
        );
        assert_eq!(parse_list(b",,,,     "), Vec::<Bytes>::new());
        assert_eq!(
            parse_list(b"\" just with starting quotation"),
            vec![b("\""), b("just"), b("with"), b("starting"), b("quotation")]
        );
        assert_eq!(
            parse_list(&b"\"longer quotation\" with \"no ending quotation"[..]),
            vec![
                b("longer quotation"),
                b("with"),
                b("\"no"),
                b("ending"),
                b("quotation"),
            ]
        );
        assert_eq!(
            parse_list(&b"this is \\\" \"not a quotation mark\""[..]),
            vec![b("this"), b("is"), b("\""), b("not a quotation mark")]
        );
        assert_eq!(parse_list(b"\n \n\nding\ndong"), vec![b("ding"), b("dong")]);

        // Other manually written cases
        assert_eq!(parse_list("a,b,,c"), vec![b("a"), b("b"), b("c")]);
        assert_eq!(parse_list("a b  c"), vec![b("a"), b("b"), b("c")]);
        assert_eq!(
            parse_list(" , a , , b,  , c , "),
            vec![b("a"), b("b"), b("c")]
        );
        assert_eq!(parse_list("a,\"b,c\" d"), vec![b("a"), b("b,c"), b("d")]);
        assert_eq!(parse_list("a,\",c"), vec![b("a"), b("\""), b("c")]);
        assert_eq!(parse_list("a,\" c\" \""), vec![b("a"), b(" c\"")]);
        assert_eq!(
            parse_list("a,\" c\" \" d"),
            vec![b("a"), b(" c"), b("\""), b("d")]
        );
    }
}
