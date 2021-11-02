/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use edenapi_types::ToApi;
use edenapi_types::ToWire;
use edenapi_types::WireToApiConversionError;
use quickcheck::Arbitrary;
use quickcheck::Gen;
use type_macros::auto_wire;

// Simulating edenapi wire crate
pub mod wire {
    pub fn is_default<T: Default + PartialEq>(t: &T) -> bool {
        t == &Default::default()
    }
}

#[auto_wire]
#[derive(
    Default,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    Clone,
    PartialEq,
    Eq
)]
struct ApiObj {
    /// Doc comment should work here
    #[id(0)]
    a: i64,
    #[id(1)]
    /// Doc comment should also work here
    b: u8,
}

#[auto_wire]
#[derive(Default, Debug, Clone, PartialEq, Eq)]
struct ComplexObj {
    #[id(1)]
    inner: ApiObj,
    #[id(2)]
    b: bool,
}

#[auto_wire]
#[derive(Clone, Debug, PartialEq, Eq)]
enum MyEnum {
    #[id(1)]
    A,
    #[id(2)]
    B,
}

impl Default for MyEnum {
    fn default() -> Self {
        Self::A
    }
}

impl Arbitrary for ApiObj {
    fn arbitrary(g: &mut Gen) -> Self {
        Self {
            a: Arbitrary::arbitrary(g),
            b: Arbitrary::arbitrary(g),
        }
    }
}

impl Arbitrary for ComplexObj {
    fn arbitrary(g: &mut Gen) -> Self {
        Self {
            inner: Arbitrary::arbitrary(g),
            b: Arbitrary::arbitrary(g),
        }
    }
}

impl Arbitrary for MyEnum {
    fn arbitrary(g: &mut Gen) -> Self {
        if Arbitrary::arbitrary(g) {
            Self::A
        } else {
            Self::B
        }
    }
}

#[test]
fn main() {
    let x = ApiObj { a: 12, b: 42 };
    let y = WireApiObj { a: 12, b: 42 };
    assert_eq!(x.clone().to_wire(), y);
    assert_eq!(x, y.clone().to_api().unwrap());
    assert_eq!(&serde_json::to_string(&y).unwrap(), r#"{"0":12,"1":42}"#);

    let x = ComplexObj { inner: x, b: true };
    let y = WireComplexObj { inner: y, b: true };
    assert_eq!(x.clone().to_wire(), y);
    assert_eq!(x, y.clone().to_api().unwrap());
    assert_eq!(
        &serde_json::to_string(&y).unwrap(),
        r#"{"1":{"0":12,"1":42},"2":true}"#
    );

    let x = MyEnum::A;
    let y = WireMyEnum::A;
    assert_eq!(x.clone().to_wire(), y);
    assert_eq!(x, y.clone().to_api().unwrap());
    assert_eq!(&serde_json::to_string(&y).unwrap(), r#""1""#);
    assert_eq!(WireMyEnum::default().to_api().unwrap(), MyEnum::A);
}
