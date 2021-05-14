/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use gotham::state::{FromState, State};
use http::header::{AsHeaderName, HeaderMap};
use permission_checker::MononokeIdentitySet;
use std::str::FromStr;

pub fn read_header_value<K, T>(state: &State, header: K) -> Option<Result<T, Error>>
where
    K: AsHeaderName,
    T: FromStr,
    <T as FromStr>::Err: std::error::Error + Send + Sync + 'static,
{
    let headers = HeaderMap::try_borrow_from(&state)?;
    let val = headers.get(header)?;
    let val = std::str::from_utf8(val.as_bytes())
        .map_err(Error::from)
        .and_then(|val| T::from_str(val).map_err(Error::from));
    Some(val)
}

pub fn is_identity_subset<'a>(
    subset_idents: impl IntoIterator<Item = &'a MononokeIdentitySet>,
    client_idents: Option<&MononokeIdentitySet>,
) -> bool {
    let client_idents = match client_idents {
        Some(idents) => idents,
        None => return false,
    };

    subset_idents
        .into_iter()
        .any(|subset_ids| subset_ids.is_subset(&client_idents))
}
