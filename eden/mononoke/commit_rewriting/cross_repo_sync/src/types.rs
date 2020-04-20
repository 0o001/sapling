/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use ref_cast::RefCast;
use std::fmt::{self, Debug, Display};
use std::hash::{Hash, Hasher};

macro_rules! generic_newtype_with_obvious_impls {
    ($name: ident) => {
        #[derive(RefCast)]
        #[repr(transparent)]
        pub struct $name<T>(pub T);

        impl<T: Debug> Debug for $name<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<T: Display> Display for $name<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<T: PartialEq> PartialEq for $name<T> {
            fn eq(&self, other: &Self) -> bool {
                self.0 == other.0
            }
        }

        impl<T: Eq> Eq for $name<T> {}

        impl<T: Copy> Copy for $name<T> {}

        impl<T: Clone> Clone for $name<T> {
            fn clone(&self) -> Self {
                Self(self.0.clone())
            }
        }

        impl<T: Hash> Hash for $name<T> {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.0.hash(state)
            }
        }
    };
}

generic_newtype_with_obvious_impls! { Large }
generic_newtype_with_obvious_impls! { Small }
generic_newtype_with_obvious_impls! { Source }
generic_newtype_with_obvious_impls! { Target }
