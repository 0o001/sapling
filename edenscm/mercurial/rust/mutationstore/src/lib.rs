// Copyright 2018 Facebook, Inc.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

extern crate byteorder;
#[macro_use]
extern crate cpython;
extern crate cpython_failure;
extern crate encoding;
extern crate failure;
extern crate mutationstore as mutstore;
extern crate types;
extern crate vlqencoding;

use byteorder::{ReadBytesExt, WriteBytesExt};
use cpython::{exc, PyBytes, PyErr, PyObject, PyResult, PyString, Python};
use cpython_failure::ResultPyErrExt;
use encoding::local_bytes_to_path;
use failure::ResultExt;
use mutstore::{MutationEntry, MutationEntryOrigin, MutationStore};
use std::cell::RefCell;
use std::io::Cursor;
use types::node::Node;
use vlqencoding::{VLQDecode, VLQEncode};

/// Supported format of bundle version.
/// Format 1 is:
///  * Single byte version: 0x01
///  * VLQ-encoded count of entries: ``count``
///  * A sequence of ``count`` entries encoded using ``MutationEntry::serialize``
const BUNDLE_FORMAT_VERSION: u8 = 1u8;

py_module_initializer!(
    mutationstore,
    initmutationstore,
    PyInit_mutationstore,
    |py, m| {
        m.add(py, "ORIGIN_COMMIT", mutstore::ORIGIN_COMMIT)?;
        m.add(py, "ORIGIN_OBSMARKER", mutstore::ORIGIN_OBSMARKER)?;
        m.add(py, "ORIGIN_SYNTHETIC", mutstore::ORIGIN_SYNTHETIC)?;
        m.add_class::<mutationentry>(py)?;
        m.add_class::<mutationstore>(py)?;
        m.add(
            py,
            "bundle",
            py_fn!(py, bundle(entries: Vec<mutationentry>)),
        )?;
        m.add(py, "unbundle", py_fn!(py, unbundle(data: PyBytes)))?;
        Ok(())
    }
);

fn bundle(py: Python, entries: Vec<mutationentry>) -> PyResult<PyBytes> {
    // Pre-allocate capacity for all the entries, plus one for the header and extra breathing room.
    let mut buf = Vec::with_capacity((entries.len() + 1) * mutstore::DEFAULT_ENTRY_SIZE);
    buf.write_u8(BUNDLE_FORMAT_VERSION)
        .map_pyerr::<exc::IOError>(py)?;
    buf.write_vlq(entries.len()).map_pyerr::<exc::IOError>(py)?;
    for entry in entries {
        let entry = entry.entry(py);
        entry.serialize(&mut buf).map_pyerr::<exc::IOError>(py)?;
    }
    Ok(PyBytes::new(py, &buf))
}

fn unbundle(py: Python, data: PyBytes) -> PyResult<Vec<mutationentry>> {
    let mut cursor = Cursor::new(data.data(py));
    let version = cursor.read_u8().map_pyerr::<exc::IOError>(py)?;
    if version != BUNDLE_FORMAT_VERSION {
        return Err(PyErr::new::<exc::IOError, _>(
            py,
            format!("Unsupported mutation format: {}", version),
        ));
    }
    let count = cursor.read_vlq().map_pyerr::<exc::IOError>(py)?;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = MutationEntry::deserialize(&mut cursor).map_pyerr::<exc::IOError>(py)?;
        entries.push(mutationentry::create_instance(py, entry)?);
    }
    Ok(entries)
}

py_class!(class mutationentry |py| {
    data entry: MutationEntry;

    def __new__(
        _cls,
        origin: u8,
        succ: &PyBytes,
        preds: Option<Vec<PyBytes>>,
        split: Option<Vec<PyBytes>>,
        op: &PyString,
        user: &PyBytes,
        time: f64,
        tz: i32,
        extra: Option<Vec<(PyBytes, PyBytes)>>
    ) -> PyResult<mutationentry> {
        let origin = MutationEntryOrigin::from_id(origin).map_pyerr::<exc::ValueError>(py)?;
        let succ = Node::from_slice(succ.data(py))
            .with_context(|e| format!("Invalid successor node: {}", e))
            .map_pyerr::<exc::ValueError>(py)?;
        let preds = {
            let mut nodes = Vec::new();
            if let Some(preds) = preds {
                for p in preds {
                    nodes.push(Node::from_slice(p.data(py))
                        .with_context(|e| format!("Invalid predecessor node: {}", e))
                        .map_pyerr::<exc::ValueError>(py)?);
                }
            }
            nodes
        };
        let split = {
            let mut nodes = Vec::new();
            if let Some(split) = split {
                for s in split {
                    nodes.push(Node::from_slice(s.data(py))
                        .with_context(|e| format!("Invalid split node: {}", e))
                        .map_pyerr::<exc::ValueError>(py)?);
                }
            }
            nodes
        };
        let op = op.to_string(py)?.into();
        let user = Box::from(user.data(py));
        let extra = {
            let mut items = Vec::new();
            if let Some(extra) = extra {
                for (k, v) in extra {
                    items.push((Box::from(k.data(py)), Box::from(v.data(py))));
                }
            }
            items
        };
        mutationentry::create_instance(py, MutationEntry {
            origin, succ, preds, split, op, user, time, tz, extra
        })
    }

    def origin(&self) -> PyResult<u8> {
        Ok(self.entry(py).origin.get_id())
    }

    def succ(&self) -> PyResult<PyBytes> {
        Ok(PyBytes::new(py, self.entry(py).succ.as_ref()))
    }

    def preds(&self) -> PyResult<Vec<PyBytes>> {
        Ok(self.entry(py).preds.iter().map(|p| PyBytes::new(py, p.as_ref())).collect())
    }

    def split(&self) -> PyResult<Vec<PyBytes>> {
        Ok(self.entry(py).split.iter().map(|s| PyBytes::new(py, s.as_ref())).collect())
    }

    def op(&self) -> PyResult<PyString> {
        Ok(PyString::new(py, self.entry(py).op.as_ref()))
    }

    def user(&self) -> PyResult<PyBytes> {
        Ok(PyBytes::new(py, self.entry(py).user.as_ref()))
    }

    def time(&self) -> PyResult<f64> {
        Ok(self.entry(py).time)
    }

    def tz(&self) -> PyResult<i32> {
        Ok(self.entry(py).tz)
    }

    def extra(&self) -> PyResult<Vec<(PyBytes, PyBytes)>> {
        Ok(self.entry(py).extra.iter().map(|(k, v)| {
            (PyBytes::new(py, k.as_ref()), PyBytes::new(py, v.as_ref()))
        }).collect())
    }
});

py_class!(class mutationstore |py| {
    data mut_store: RefCell<MutationStore>;

    def __new__(_cls, path: &PyBytes) -> PyResult<mutationstore> {
        let path = local_bytes_to_path(path.data(py))
            .map_pyerr::<exc::ValueError>(py)?;
        let ms = MutationStore::open(path).map_pyerr::<exc::ValueError>(py)?;
        mutationstore::create_instance(py, RefCell::new(ms))
    }

    def add(&self, entry: &mutationentry) -> PyResult<PyObject> {
        let mut ms = self.mut_store(py).borrow_mut();
        ms.add(entry.entry(py)).map_pyerr::<exc::ValueError>(py)?;
        Ok(py.None())
    }

    def flush(&self) -> PyResult<PyObject> {
        let mut ms = self.mut_store(py).borrow_mut();
        ms.flush().map_pyerr::<exc::ValueError>(py)?;
        Ok(py.None())
    }

    def has(&self, succ: &PyBytes) -> PyResult<bool> {
        let succ = Node::from_slice(succ.data(py)).map_pyerr::<exc::ValueError>(py)?;
        let ms = self.mut_store(py).borrow();
        let entry = ms.get(succ).map_pyerr::<exc::IOError>(py)?;
        Ok(entry.is_some())
    }

    def get(&self, succ: &PyBytes) -> PyResult<Option<mutationentry>> {
        let succ = Node::from_slice(succ.data(py)).map_pyerr::<exc::ValueError>(py)?;
        let ms = self.mut_store(py).borrow();
        let entry = ms.get(succ).map_pyerr::<exc::IOError>(py)?;
        let entry = match entry {
            Some(entry) => Some(mutationentry::create_instance(py, entry)?),
            None => None,
        };
        Ok(entry)
    }

    def getsplithead(&self, node: &PyBytes) -> PyResult<Option<PyBytes>> {
        let node = Node::from_slice(node.data(py)).map_pyerr::<exc::ValueError>(py)?;
        let ms = self.mut_store(py).borrow();
        let entry = ms.get_split_head(node).map_pyerr::<exc::IOError>(py)?;
        let succ = match entry {
            Some(entry) => Some(PyBytes::new(py, entry.succ.as_ref())),
            None => None,
        };
        Ok(succ)
    }

    def getsuccessorssets(&self, node: &PyBytes) -> PyResult<Vec<Vec<PyBytes>>> {
        let node = Node::from_slice(node.data(py)).map_pyerr::<exc::ValueError>(py)?;
        let ms = self.mut_store(py).borrow();
        let ssets = ms.get_successors_sets(node).map_pyerr::<exc::IOError>(py)?;
        let mut pyssets = Vec::with_capacity(ssets.len());
        for sset in ssets.into_iter() {
            let mut pysset = Vec::with_capacity(sset.len());
            for succ in sset.into_iter() {
                pysset.push(PyBytes::new(py, succ.as_ref()));
            }
            pyssets.push(pysset);
        }
        Ok(pyssets)
    }
});
