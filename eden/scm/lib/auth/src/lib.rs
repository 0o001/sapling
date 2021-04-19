/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::{collections::HashMap, path::PathBuf, str};

use anyhow::{Error, Result};
use indexmap::IndexMap;
use thiserror::Error;
use url::Url;

use configmodel::{Config, Text};
use util::path::expand_path;

pub mod x509;

pub use x509::{check_certs, X509Error};

#[derive(Debug, Error)]
#[error("Certificate(s) or private key(s) not found: {missing:?}")]
pub struct MissingCerts {
    missing: Vec<PathBuf>,
}

/// A group of client authentiation settings from the user's config.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthGroup {
    pub name: String,
    pub prefix: String,
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
    pub cacerts: Option<PathBuf>,
    pub username: Option<String>,
    pub schemes: Vec<String>,
    pub priority: i32,
    pub extras: HashMap<String, String>,
}

impl AuthGroup {
    fn new(group: &str, mut settings: HashMap<&str, Text>) -> Result<Self> {
        let name = group.into();

        let mut prefix = settings
            .remove("prefix")
            .map(|s| s.to_string())
            .ok_or_else(|| Error::msg("auth prefix missing"))?;

        let cert = settings
            .remove("cert")
            .filter(|s| !s.is_empty())
            .map(expand_path);
        let key = settings
            .remove("key")
            .filter(|s| !s.is_empty())
            .map(expand_path);
        let cacerts = settings
            .remove("cacerts")
            .filter(|s| !s.is_empty())
            .map(expand_path);

        let username = settings.remove("username").map(|s| s.to_string());

        // If the URL prefix for this group has a scheme specified, use that
        // and ignore the contents of the "schemes" field for this group.
        let schemes = if let Some(i) = prefix.find("://") {
            let _ = settings.remove("schemes");
            let scheme = prefix[..i].into();
            prefix = prefix[i + 3..].into();
            vec![scheme]
        } else {
            // Default to HTTPS if no schemes are specified in either the
            // prefix or schemes field.
            settings.remove("schemes").map_or_else(
                || vec!["https".into()],
                |line| line.split(' ').map(String::from).collect(),
            )
        };

        let priority = settings
            .remove("priority")
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or_default();

        let extras = settings
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>();

        Ok(Self {
            name,
            prefix,
            cert,
            key,
            cacerts,
            username,
            schemes,
            priority,
            extras,
        })
    }
}

#[derive(Clone, Debug)]
pub struct AuthSection {
    groups: Vec<AuthGroup>,
}

impl AuthSection {
    /// Parse the `[auth]` section of a Mercurial config into a map
    /// of grouped auth settings.
    ///
    /// The keys of the resulting HashMap are group names from the config;
    /// i.e., the first component of a key of the form `group.setting`.
    /// All keys in the auth section are expected to follow this format.
    ///
    /// Values are parsed `Auth` structs containing all of the values
    /// found for the given grouping.
    pub fn from_config(config: &dyn Config) -> Self {
        // Use an IndexMap to preserve ordering; needed to correctly handle precedence.
        let mut groups = IndexMap::new();

        let keys = config.keys("auth");
        for key in &keys {
            // Skip keys that aren't valid UTF-8 or that don't match
            // the expected auth key format of `group.setting`.
            let (group, setting) = match key.find('.') {
                Some(i) => (&key[..i], &key[i + 1..]),
                None => continue,
            };
            if let Some(value) = config.get("auth", key) {
                groups
                    .entry(group)
                    .or_insert_with(HashMap::new)
                    .insert(setting, value);
            }
        }

        let groups = groups
            .into_iter()
            .filter_map(|(group, settings)| AuthGroup::new(group, settings).ok())
            .collect();

        Self { groups }
    }

    /// Find the best matching auth group for the given URL.
    pub fn best_match_for(&self, url: &Url) -> Result<Option<AuthGroup>, MissingCerts> {
        let mut best: Option<&AuthGroup> = None;
        let mut missing = Vec::new();

        let scheme = url.scheme().to_string();
        let username = url.username();
        let url_suffix = strip_scheme_and_user(&url);

        'groups: for group in &self.groups {
            if !group.schemes.contains(&scheme) {
                continue;
            }

            // If the URL contains a username, the entry's username must
            // match if the entry's username field is non-None.
            if !username.is_empty() {
                match group.username {
                    Some(ref u) if u != username => continue,
                    _ => {}
                }
            }

            if group.prefix != "*" && !url_suffix.starts_with(&group.prefix) {
                continue;
            }

            // If there is an existing candidate, check whether the current
            // auth entry is a more specific match.
            if let Some(ref best) = best {
                // Take the entry with the longer prefix.
                if group.prefix.len() < best.prefix.len() {
                    continue;
                } else if group.prefix.len() == best.prefix.len() {
                    // If prefixes are the same, break the tie using priority.
                    if group.priority < best.priority {
                        continue;
                    // If the priorities are the same, prefer entries with usernames.
                    } else if group.priority == best.priority
                        && best.username.is_some()
                        && group.username.is_none()
                    {
                        continue;
                    }
                }
            }

            // Skip this group is any of the files are missing.
            for (label, path) in &[
                ("client certificate", &group.cert),
                ("private key", &group.key),
                ("CA certificate bundle", &group.cacerts),
            ] {
                match path {
                    Some(path) if !path.is_file() => {
                        tracing::debug!(
                            "Ignoring [auth] group {:?} because of missing {}: {:?}",
                            &group.name,
                            &label,
                            &path
                        );
                        missing.push(path.to_path_buf());
                        continue 'groups;
                    }
                    _ => {}
                }
            }

            best = Some(group);
        }

        if let Some(best) = best {
            Ok(Some(best.clone()))
        } else if !missing.is_empty() {
            Err(MissingCerts { missing })
        } else {
            Ok(None)
        }
    }
}

/// Given a URL, strip off the scheme and username if present.
fn strip_scheme_and_user(url: &Url) -> String {
    let url = url.as_str();
    // Find the position immediately after the '@' if a username is present.
    // or after the scheme otherwise.
    let pos = url
        .find('@')
        .map(|i| i + 1)
        .or_else(|| url.find("://").map(|i| i + 3));

    match pos {
        Some(i) => &url[i..],
        None => url,
    }
    .to_string()
}

#[cfg(test)]
mod test {
    use super::*;

    use configparser::config::ConfigSet;
    use configparser::config::Options;

    #[test]
    fn test_auth_groups() {
        let mut config = ConfigSet::new();
        let _errors = config.parse(
            "[auth]\n\
             foo.prefix = foo.com\n\
             foo.cert = /foo/cert\n\
             foo.key = /foo/key\n\
             foo.cacerts = /foo/cacerts\n\
             bar.prefix = bar.com\n\
             bar.cert = /bar/cert\n\
             bar.key = /bar/key\n\
             baz.cert = /baz/cert\n\
             baz.key = /baz/key\n\
             foo.username = user\n\
             foo.schemes = http https\n\
             foo.priority = 1\n
             ",
            &Options::default(),
        );
        let groups = AuthSection::from_config(&config).groups;

        // Only 2 groups because "baz" is missing the required "prefix" setting.
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0],
            AuthGroup {
                name: "foo".into(),
                prefix: "foo.com".into(),
                cert: Some("/foo/cert".into()),
                key: Some("/foo/key".into()),
                cacerts: Some("/foo/cacerts".into()),
                username: Some("user".into()),
                schemes: vec!["http".into(), "https".into()],
                priority: 1,
                extras: HashMap::new(),
            }
        );
        assert_eq!(
            groups[1],
            AuthGroup {
                name: "bar".into(),
                prefix: "bar.com".into(),
                cert: Some("/bar/cert".into()),
                key: Some("/bar/key".into()),
                cacerts: None,
                username: None,
                schemes: vec!["https".into()],
                priority: 0,
                extras: HashMap::new(),
            }
        );
    }

    #[test]
    fn test_strip_scheme_and_user() -> Result<()> {
        let url = "https://example.com/".parse()?;
        let stripped = strip_scheme_and_user(&url);
        assert_eq!(stripped, "example.com/");

        let url = "https://user@example.com:433/some/path?query=1".parse()?;
        let stripped = strip_scheme_and_user(&url);
        assert_eq!(stripped, "example.com:433/some/path?query=1");

        Ok(())
    }

    #[test]
    fn test_best_match_for() -> Result<()> {
        let mut config = ConfigSet::new();
        let _errors = config.parse(
            "[auth]\n\
             default.prefix = *\n\
             a.prefix = example.com\n\
             a.schemes = http https\n\
             b.prefix = foo.com/bar\n\
             c.prefix = foo.com/bar/baz\n\
             d.prefix = bar.com\n\
             d.priority = 1\n\
             e.prefix = bar.com\n\
             e.username = e_user\n\
             f.prefix = baz.com\n\
             f.username = f_user\n\
             g.prefix = baz.com\n\
             h.prefix = example.net\n\
             h.username = h_user\n\
             i.prefix = example.net\n\
             i.username = i_user\n\
             j.prefix = invalid.com\n\
             j.cert = /does/not/exist\n\
             ",
            &Options::default(),
        );
        let auth = AuthSection::from_config(&config);

        // Basic case: an exact match.
        let group = auth
            .best_match_for(&"http://example.com".parse()?)?
            .unwrap();
        assert_eq!(group.name, "a");

        // Scheme mismatch.
        let group = auth.best_match_for(&"ftp://example.com".parse()?)?;
        assert!(group.is_none());

        // Given URL's hosts matches a config prefix, but doesn't match
        // the entire prefix.
        let group = auth.best_match_for(&"https://foo.com.".parse()?)?.unwrap();
        assert_eq!(group.name, "default");

        // Matching the entire prefix works as expected.
        let group = auth
            .best_match_for(&"https://foo.com/bar".parse()?)?
            .unwrap();
        assert_eq!(group.name, "b");

        // A more specific prefix wins.
        let group = auth
            .best_match_for(&"https://foo.com/bar/baz".parse()?)?
            .unwrap();
        assert_eq!(group.name, "c");

        // Still matches even if the URL has other components in it.
        let group = auth
            .best_match_for(&"https://foo.com/bar/baz/dir?query=1#fragment".parse()?)?
            .unwrap();
        assert_eq!(group.name, "c");

        // There are two entries matching this prefix, but one has higher priority.
        let group = auth.best_match_for(&"https://bar.com".parse()?)?.unwrap();
        assert_eq!(group.name, "d");

        // Even if another entry has a username match, the higher priority should win.
        let group = auth
            .best_match_for(&"https://e_user@bar.com".parse()?)?
            .unwrap();
        assert_eq!(group.name, "d");

        // Even if no user is specified in the URL, the entry with a username should
        // take precedence all else being equal.
        let group = auth.best_match_for(&"https://baz.com".parse()?)?.unwrap();
        assert_eq!(group.name, "f");

        // If all else fails, later entries take precedence over earlier ones.
        // Even if no user is specified in the URL, the entry with a username should
        // take precedence all else being equal.
        let group = auth
            .best_match_for(&"https://example.net".parse()?)?
            .unwrap();
        assert_eq!(group.name, "i");

        // If the cert of key is missing, the entry shouldn't match.
        let group = auth
            .best_match_for(&"https://invalid.com".parse()?)?
            .unwrap();
        assert_eq!(group.name, "default");

        Ok(())
    }

    #[test]
    fn test_extras() -> Result<()> {
        let mut config = ConfigSet::new();
        let _errors = config.parse(
            "[auth]\n\
             foo.prefix = foo.com\n\
             foo.username = user\n\
             foo.hello = world\n\
             foo.bar = baz\n\
             ",
            &Options::default(),
        );
        let auth = AuthSection::from_config(&config);

        let group = auth.best_match_for(&"https://foo.com".parse()?)?.unwrap();

        assert_eq!(group.extras.get("hello").unwrap(), "world");
        assert_eq!(group.extras.get("bar").unwrap(), "baz");
        assert_eq!(group.extras.get("username"), None);

        Ok(())
    }
}
