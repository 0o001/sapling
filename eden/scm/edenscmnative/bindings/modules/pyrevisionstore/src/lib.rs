/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! revisionstore - Python interop layer for a Mercurial data and history store

#![allow(non_camel_case_types)]

use std::{
    convert::TryInto,
    fs::read_dir,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{format_err, Error};
use cpython::*;
use parking_lot::RwLock;

use cpython_ext::{PyErr, PyPath, ResultPyErrExt};
use pyconfigparser::config;
use revisionstore::{
    repack::{filter_incrementalpacks, list_packs, repack_datapacks, repack_historypacks},
    ContentStore, ContentStoreBuilder, CorruptionPolicy, DataPack, DataPackStore, DataPackVersion,
    DataStore, Delta, HistoryPack, HistoryPackStore, HistoryPackVersion, HistoryStore,
    IndexedLogDataStore, IndexedLogHistoryStore, IndexedlogRepair, LocalStore, Metadata,
    MetadataStore, MetadataStoreBuilder, MutableDataPack, MutableDeltaStore, MutableHistoryPack,
    MutableHistoryStore, RemoteDataStore, RemoteHistoryStore, RemoteStore,
};
use types::{Key, NodeInfo};

use crate::{
    datastorepyext::{
        DataStorePyExt, IterableDataStorePyExt, MutableDeltaStorePyExt, RemoteDataStorePyExt,
    },
    historystorepyext::{
        HistoryStorePyExt, IterableHistoryStorePyExt, MutableHistoryStorePyExt,
        RemoteHistoryStorePyExt,
    },
    pythonutil::from_key,
};

mod datastorepyext;
mod historystorepyext;
mod pythondatastore;
mod pythonutil;

type Result<T, E = Error> = std::result::Result<T, E>;

pub use crate::pythondatastore::PythonDataStore;

pub fn init_module(py: Python, package: &str) -> PyResult<PyModule> {
    let name = [package, "revisionstore"].join(".");
    let m = PyModule::new(py, &name)?;
    m.add_class::<datapack>(py)?;
    m.add_class::<datapackstore>(py)?;
    m.add_class::<historypack>(py)?;
    m.add_class::<historypackstore>(py)?;
    m.add_class::<indexedlogdatastore>(py)?;
    m.add_class::<indexedloghistorystore>(py)?;
    m.add_class::<mutabledeltastore>(py)?;
    m.add_class::<mutablehistorystore>(py)?;
    m.add_class::<pyremotestore>(py)?;
    m.add_class::<contentstore>(py)?;
    m.add_class::<metadatastore>(py)?;
    m.add(
        py,
        "repackdatapacks",
        py_fn!(py, repackdata(packpath: PyPath, outdir: PyPath)),
    )?;
    m.add(
        py,
        "repackincrementaldatapacks",
        py_fn!(py, incremental_repackdata(packpath: PyPath, outdir: PyPath)),
    )?;
    m.add(
        py,
        "repackhistpacks",
        py_fn!(py, repackhist(packpath: PyPath, outdir: PyPath)),
    )?;
    m.add(
        py,
        "repackincrementalhistpacks",
        py_fn!(py, incremental_repackhist(packpath: PyPath, outdir: PyPath)),
    )?;
    Ok(m)
}

/// Helper function to de-serialize and re-serialize from and to Python objects.
fn repack_pywrapper(
    py: Python,
    path: PyPath,
    outdir: PyPath,
    repacker: impl FnOnce(PathBuf, PathBuf) -> Result<PathBuf>,
) -> PyResult<PyPath> {
    repacker(path.to_path_buf(), outdir.to_path_buf())
        .and_then(|p| p.try_into())
        .map_pyerr(py)
}

/// Merge all the datapacks into one big datapack. Returns the fullpath of the resulting datapack.
fn repackdata(py: Python, packpath: PyPath, outdir_py: PyPath) -> PyResult<PyPath> {
    repack_pywrapper(py, packpath, outdir_py, |dir, outdir| {
        repack_datapacks(list_packs(&dir, "datapack")?.iter(), &outdir)
    })
}

/// Merge all the history packs into one big historypack. Returns the fullpath of the resulting
/// histpack.
fn repackhist(py: Python, packpath: PyPath, outdir_py: PyPath) -> PyResult<PyPath> {
    repack_pywrapper(py, packpath, outdir_py, |dir, outdir| {
        repack_historypacks(list_packs(&dir, "histpack")?.iter(), &outdir)
    })
}

/// Perform an incremental repack of data packs.
fn incremental_repackdata(py: Python, packpath: PyPath, outdir_py: PyPath) -> PyResult<PyPath> {
    repack_pywrapper(py, packpath, outdir_py, |dir, outdir| {
        repack_datapacks(
            filter_incrementalpacks(list_packs(&dir, "datapack")?, "datapack")?.iter(),
            &outdir,
        )
    })
}

/// Perform an incremental repack of history packs.
fn incremental_repackhist(py: Python, packpath: PyPath, outdir_py: PyPath) -> PyResult<PyPath> {
    repack_pywrapper(py, packpath, outdir_py, |dir, outdir| {
        repack_historypacks(
            filter_incrementalpacks(list_packs(&dir, "histpack")?, "histpack")?.iter(),
            &outdir,
        )
    })
}

py_class!(class datapack |py| {
    data store: Box<DataPack>;

    def __new__(
        _cls,
        path: PyPath
    ) -> PyResult<datapack> {
        datapack::create_instance(
            py,
            Box::new(DataPack::new(&path).map_pyerr(py)?),
        )
    }

    def path(&self) -> PyResult<PyPath> {
        self.store(py).base_path().try_into().map_pyerr(py)
    }

    def packpath(&self) -> PyResult<PyPath> {
        self.store(py).pack_path().try_into().map_pyerr(py)
    }

    def indexpath(&self) -> PyResult<PyPath> {
        self.store(py).index_path().try_into().map_pyerr(py)
    }

    def get(&self, name: PyPath, node: &PyBytes) -> PyResult<PyBytes> {
        let store = self.store(py);
        store.get_py(py, &name, node)
    }

    def getdelta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyObject> {
        let store = self.store(py);
        store.get_delta_py(py, &name, node)
    }

    def getdeltachain(&self, name: PyPath, node: &PyBytes) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_delta_chain_py(py, &name, node)
    }

    def getmeta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyDict> {
        let store = self.store(py);
        store.get_meta_py(py, &name, node)
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_missing_py(py, &mut keys.iter(py)?)
    }

    def iterentries(&self) -> PyResult<Vec<PyTuple>> {
        let store = self.store(py);
        store.iter_py(py)
    }
});

/// Scan the filesystem for files with `extensions`, and compute their size.
fn compute_store_size<P: AsRef<Path>>(
    storepath: P,
    extensions: Vec<&str>,
) -> Result<(usize, usize)> {
    let dirents = read_dir(storepath)?;

    assert_eq!(extensions.len(), 2);

    let mut count = 0;
    let mut size = 0;

    for dirent in dirents {
        let dirent = dirent?;
        let path = dirent.path();

        if let Some(file_ext) = path.extension() {
            for extension in &extensions {
                if extension == &file_ext {
                    size += dirent.metadata()?.len();
                    count += 1;
                    break;
                }
            }
        }
    }

    // We did count the indexes too, but we do not want them counted.
    count /= 2;

    Ok((size as usize, count))
}

py_class!(class datapackstore |py| {
    data store: Box<DataPackStore>;
    data path: PathBuf;

    def __new__(_cls, path: PyPath, deletecorruptpacks: bool = false) -> PyResult<datapackstore> {
        let corruption_policy = if deletecorruptpacks {
            CorruptionPolicy::REMOVE
        } else {
            CorruptionPolicy::IGNORE
        };

        datapackstore::create_instance(py, Box::new(DataPackStore::new(&path, corruption_policy)), path.to_path_buf())
    }

    def get(&self, name: PyPath, node: &PyBytes) -> PyResult<PyBytes> {
        self.store(py).get_py(py, &name, node)
    }

    def getmeta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyDict> {
        self.store(py).get_meta_py(py, &name, node)
    }

    def getdelta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyObject> {
        self.store(py).get_delta_py(py, &name, node)
    }

    def getdeltachain(&self, name: PyPath, node: &PyBytes) -> PyResult<PyList> {
        self.store(py).get_delta_chain_py(py, &name, node)
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        self.store(py).get_missing_py(py, &mut keys.iter(py)?)
    }

    def markforrefresh(&self) -> PyResult<PyObject> {
        self.store(py).force_rescan();
        Ok(Python::None(py))
    }

    def getmetrics(&self) -> PyResult<PyDict> {
        let (size, count) = match compute_store_size(self.path(py), vec!["datapack", "dataidx"]) {
            Ok((size, count)) => (size, count),
            Err(_) => (0, 0),
        };

        let res = PyDict::new(py);
        res.set_item(py, "numpacks", count)?;
        res.set_item(py, "totalpacksize", size)?;
        Ok(res)
    }
});

py_class!(class historypack |py| {
    data store: Box<HistoryPack>;

    def __new__(
        _cls,
        path: PyPath
    ) -> PyResult<historypack> {
        historypack::create_instance(
            py,
            Box::new(HistoryPack::new(&path).map_pyerr(py)?),
        )
    }

    def path(&self) -> PyResult<PyPath> {
        self.store(py).base_path().try_into().map_pyerr(py)
    }

    def packpath(&self) -> PyResult<PyPath> {
        self.store(py).pack_path().try_into().map_pyerr(py)
    }

    def indexpath(&self) -> PyResult<PyPath> {
        self.store(py).index_path().try_into().map_pyerr(py)
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_missing_py(py, &mut keys.iter(py)?)
    }

    def getnodeinfo(&self, name: PyPath, node: &PyBytes) -> PyResult<PyTuple> {
        let store = self.store(py);
        store.get_node_info_py(py, &name, node)
    }

    def iterentries(&self) -> PyResult<Vec<PyTuple>> {
        let store = self.store(py);
        store.iter_py(py)
    }
});

py_class!(class historypackstore |py| {
    data store: Box<HistoryPackStore>;
    data path: PathBuf;

    def __new__(_cls, path: PyPath, deletecorruptpacks: bool = false) -> PyResult<historypackstore> {
        let corruption_policy = if deletecorruptpacks {
            CorruptionPolicy::REMOVE
        } else {
            CorruptionPolicy::IGNORE
        };

        historypackstore::create_instance(py, Box::new(HistoryPackStore::new(&path, corruption_policy)), path.to_path_buf())
    }

    def getnodeinfo(&self, name: PyPath, node: &PyBytes) -> PyResult<PyTuple> {
        self.store(py).get_node_info_py(py, &name, node)
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        self.store(py).get_missing_py(py, &mut keys.iter(py)?)
    }

    def markforrefresh(&self) -> PyResult<PyObject> {
        self.store(py).force_rescan();
        Ok(Python::None(py))
    }

    def getmetrics(&self) -> PyResult<PyDict> {
        let (size, count) = match compute_store_size(self.path(py), vec!["histpack", "histidx"]) {
            Ok((size, count)) => (size, count),
            Err(_) => (0, 0),
        };

        let res = PyDict::new(py);
        res.set_item(py, "numpacks", count)?;
        res.set_item(py, "totalpacksize", size)?;
        Ok(res)
    }
});

py_class!(class indexedlogdatastore |py| {
    data store: Box<IndexedLogDataStore>;

    def __new__(_cls, path: PyPath) -> PyResult<indexedlogdatastore> {
        indexedlogdatastore::create_instance(
            py,
            Box::new(IndexedLogDataStore::new(&path).map_pyerr(py)?),
        )
    }

    @staticmethod
    def repair(path: PyPath) -> PyResult<PyUnicode> {
        py.allow_threads(|| IndexedLogDataStore::repair(path)).map_pyerr(py).map(|s| PyUnicode::new(py, &s))
    }

    def getdelta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyObject> {
        let store = self.store(py);
        store.get_delta_py(py, &name, node)
    }

    def getdeltachain(&self, name: PyPath, node: &PyBytes) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_delta_chain_py(py, &name, node)
    }

    def getmeta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyDict> {
        let store = self.store(py);
        store.get_meta_py(py, &name, node)
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_missing_py(py, &mut keys.iter(py)?)
    }

    def markforrefresh(&self) -> PyResult<PyObject> {
        let store = self.store(py);
        store.flush_py(py)?;
        Ok(Python::None(py))
    }

    def iterentries(&self) -> PyResult<Vec<PyTuple>> {
        let store = self.store(py);
        store.iter_py(py)
    }
});

py_class!(class indexedloghistorystore |py| {
    data store: Box<IndexedLogHistoryStore>;

    def __new__(_cls, path: PyPath) -> PyResult<indexedloghistorystore> {
        indexedloghistorystore::create_instance(
            py,
            Box::new(IndexedLogHistoryStore::new(&path).map_pyerr(py)?),
        )
    }

    @staticmethod
    def repair(path: PyPath) -> PyResult<PyUnicode> {
        IndexedLogHistoryStore::repair(path).map_pyerr(py).map(|s| PyUnicode::new(py, &s))
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_missing_py(py, &mut keys.iter(py)?)
    }

    def getnodeinfo(&self, name: PyPath, node: &PyBytes) -> PyResult<PyTuple> {
        let store = self.store(py);
        store.get_node_info_py(py, &name, node)
    }

    def markforrefresh(&self) -> PyResult<PyObject> {
        let store = self.store(py);
        store.flush_py(py)?;
        Ok(Python::None(py))
    }

    def iterentries(&self) -> PyResult<Vec<PyTuple>> {
        let store = self.store(py);
        store.iter_py(py)
    }
});

fn make_mutabledeltastore(
    packfilepath: Option<PyPath>,
    indexedlogpath: Option<PyPath>,
) -> Result<Box<dyn MutableDeltaStore + Send>> {
    let store: Box<dyn MutableDeltaStore + Send> = if let Some(packfilepath) = packfilepath {
        Box::new(MutableDataPack::new(packfilepath, DataPackVersion::One)?)
    } else if let Some(indexedlogpath) = indexedlogpath {
        Box::new(IndexedLogDataStore::new(indexedlogpath)?)
    } else {
        return Err(format_err!("Foo"));
    };
    Ok(store)
}

py_class!(pub class mutabledeltastore |py| {
    data store: Box<dyn MutableDeltaStore>;

    def __new__(_cls, packfilepath: Option<PyPath> = None, indexedlogpath: Option<PyPath> = None) -> PyResult<mutabledeltastore> {
        let store = make_mutabledeltastore(packfilepath, indexedlogpath).map_pyerr(py)?;
        mutabledeltastore::create_instance(py, store)
    }

    def add(&self, name: PyPath, node: &PyBytes, deltabasenode: &PyBytes, delta: &PyBytes, metadata: Option<PyDict> = None) -> PyResult<PyObject> {
        let store = self.store(py);
        store.add_py(py, &name, node, deltabasenode, delta, metadata)
    }

    def flush(&self) -> PyResult<Option<PyPath>> {
        let store = self.store(py);
        store.flush_py(py)
    }

    def getdelta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyObject> {
        let store = self.store(py);
        store.get_delta_py(py, &name, node)
    }

    def getdeltachain(&self, name: PyPath, node: &PyBytes) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_delta_chain_py(py, &name, node)
    }

    def getmeta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyDict> {
        let store = self.store(py);
        store.get_meta_py(py, &name, node)
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_missing_py(py, &mut keys.iter(py)?)
    }
});

impl DataStore for mutabledeltastore {
    fn get(&self, key: &Key) -> Result<Option<Vec<u8>>> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).get(key)
    }

    fn get_delta(&self, key: &Key) -> Result<Option<Delta>> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).get_delta(key)
    }

    fn get_delta_chain(&self, key: &Key) -> Result<Option<Vec<Delta>>> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).get_delta_chain(key)
    }

    fn get_meta(&self, key: &Key) -> Result<Option<Metadata>> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).get_meta(key)
    }
}

impl LocalStore for mutabledeltastore {
    fn get_missing(&self, keys: &[Key]) -> Result<Vec<Key>> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).get_missing(keys)
    }
}

impl MutableDeltaStore for mutabledeltastore {
    fn add(&self, delta: &Delta, metadata: &Metadata) -> Result<()> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).add(delta, metadata)
    }

    fn flush(&self) -> Result<Option<PathBuf>> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).flush()
    }
}

fn make_mutablehistorystore(
    packfilepath: Option<PyPath>,
) -> Result<Box<dyn MutableHistoryStore + Send>> {
    let store: Box<dyn MutableHistoryStore + Send> = if let Some(packfilepath) = packfilepath {
        Box::new(MutableHistoryPack::new(
            packfilepath,
            HistoryPackVersion::One,
        )?)
    } else {
        return Err(format_err!("No packfile path passed in"));
    };

    Ok(store)
}

py_class!(pub class mutablehistorystore |py| {
    data store: Box<dyn MutableHistoryStore>;

    def __new__(_cls, packfilepath: Option<PyPath>) -> PyResult<mutablehistorystore> {
        let store = make_mutablehistorystore(packfilepath).map_pyerr(py)?;
        mutablehistorystore::create_instance(py, store)
    }

    def add(&self, name: PyPath, node: &PyBytes, p1: &PyBytes, p2: &PyBytes, linknode: &PyBytes, copyfrom: Option<PyPath>) -> PyResult<PyObject> {
        let store = self.store(py);
        store.add_py(py, &name, node, p1, p2, linknode, copyfrom.as_ref())
    }

    def flush(&self) -> PyResult<Option<PyPath>> {
        let store = self.store(py);
        store.flush_py(py)
    }

    def getnodeinfo(&self, name: PyPath, node: &PyBytes) -> PyResult<PyTuple> {
        let store = self.store(py);
        store.get_node_info_py(py, &name, node)
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_missing_py(py, &mut keys.iter(py)?)
    }
});

impl HistoryStore for mutablehistorystore {
    fn get_node_info(&self, key: &Key) -> Result<Option<NodeInfo>> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).get_node_info(key)
    }
}

impl LocalStore for mutablehistorystore {
    fn get_missing(&self, keys: &[Key]) -> Result<Vec<Key>> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).get_missing(keys)
    }
}

impl MutableHistoryStore for mutablehistorystore {
    fn add(&self, key: &Key, info: &NodeInfo) -> Result<()> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).add(key, info)
    }

    fn flush(&self) -> Result<Option<PathBuf>> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        self.store(py).flush()
    }
}

struct PyRemoteStoreInner {
    py_store: PyObject,
    datastore: Option<mutabledeltastore>,
    historystore: Option<mutablehistorystore>,
}

#[derive(Clone)]
struct PyRemoteStore {
    inner: Arc<RwLock<PyRemoteStoreInner>>,
}

impl PyRemoteStore {
    fn prefetch(&self, keys: &[Key]) -> Result<()> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        let keys = keys
            .into_iter()
            .map(|key| from_key(py, &key))
            .collect::<Vec<_>>();

        let inner = self.inner.read();
        inner
            .py_store
            .call_method(
                py,
                "prefetch",
                (
                    inner.datastore.clone_ref(py),
                    inner.historystore.clone_ref(py),
                    keys,
                ),
                None,
            )
            .map_err(|e| PyErr::from(e))?;
        Ok(())
    }
}

struct PyRemoteDataStore(PyRemoteStore);
struct PyRemoteHistoryStore(PyRemoteStore);

impl RemoteStore for PyRemoteStore {
    fn datastore(&self, store: Box<dyn MutableDeltaStore>) -> Arc<dyn RemoteDataStore> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        let mut inner = self.inner.write();
        inner.datastore = Some(mutabledeltastore::create_instance(py, store).unwrap());

        Arc::new(PyRemoteDataStore(self.clone()))
    }

    fn historystore(&self, store: Box<dyn MutableHistoryStore>) -> Arc<dyn RemoteHistoryStore> {
        let gil = Python::acquire_gil();
        let py = gil.python();

        let mut inner = self.inner.write();
        inner.historystore = Some(mutablehistorystore::create_instance(py, store).unwrap());

        Arc::new(PyRemoteHistoryStore(self.clone()))
    }
}

impl RemoteDataStore for PyRemoteDataStore {
    fn prefetch(&self, keys: &[Key]) -> Result<()> {
        self.0.prefetch(keys)
    }
}

impl DataStore for PyRemoteDataStore {
    fn get(&self, _key: &Key) -> Result<Option<Vec<u8>>> {
        unreachable!();
    }

    fn get_delta(&self, key: &Key) -> Result<Option<Delta>> {
        match self.prefetch(&[key.clone()]) {
            Ok(()) => self
                .0
                .inner
                .read()
                .datastore
                .as_ref()
                .unwrap()
                .get_delta(key),
            Err(_) => Ok(None),
        }
    }

    fn get_delta_chain(&self, key: &Key) -> Result<Option<Vec<Delta>>> {
        match self.prefetch(&[key.clone()]) {
            Ok(()) => self
                .0
                .inner
                .read()
                .datastore
                .as_ref()
                .unwrap()
                .get_delta_chain(key),
            Err(_) => Ok(None),
        }
    }

    fn get_meta(&self, key: &Key) -> Result<Option<Metadata>> {
        match self.prefetch(&[key.clone()]) {
            Ok(()) => self
                .0
                .inner
                .read()
                .datastore
                .as_ref()
                .unwrap()
                .get_meta(key),
            Err(_) => Ok(None),
        }
    }
}

impl LocalStore for PyRemoteDataStore {
    fn get_missing(&self, keys: &[Key]) -> Result<Vec<Key>> {
        Ok(keys.to_vec())
    }
}

impl RemoteHistoryStore for PyRemoteHistoryStore {
    fn prefetch(&self, keys: &[Key]) -> Result<()> {
        self.0.prefetch(keys)
    }
}

impl HistoryStore for PyRemoteHistoryStore {
    fn get_node_info(&self, key: &Key) -> Result<Option<NodeInfo>> {
        match self.prefetch(&[key.clone()]) {
            Ok(()) => self
                .0
                .inner
                .read()
                .historystore
                .as_ref()
                .unwrap()
                .get_node_info(key),
            Err(_) => Ok(None),
        }
    }
}

impl LocalStore for PyRemoteHistoryStore {
    fn get_missing(&self, keys: &[Key]) -> Result<Vec<Key>> {
        Ok(keys.to_vec())
    }
}

py_class!(class pyremotestore |py| {
    data remote: PyRemoteStore;

    def __new__(_cls, py_store: PyObject) -> PyResult<pyremotestore> {
        let store = PyRemoteStore { inner: Arc::new(RwLock::new(PyRemoteStoreInner { py_store, datastore: None, historystore: None })) };
        pyremotestore::create_instance(py, store)
    }
});

impl pyremotestore {
    fn into_inner(&self, py: Python) -> PyRemoteStore {
        self.remote(py).clone()
    }
}

py_class!(class contentstore |py| {
    data store: ContentStore;

    def __new__(_cls, path: Option<PyPath>, config: config, remote: pyremotestore) -> PyResult<contentstore> {
        let remotestore = remote.into_inner(py);
        let config = config.get_cfg(py);

        let mut builder = ContentStoreBuilder::new(&config).remotestore(Box::new(remotestore));

        builder = if let Some(path) = path {
            builder.local_path(path)
        } else {
            builder.no_local_store()
        };

        let contentstore = builder.build().map_pyerr(py)?;
        contentstore::create_instance(py, contentstore)
    }

    def get(&self, name: PyPath, node: &PyBytes) -> PyResult<PyBytes> {
        let store = self.store(py);
        store.get_py(py, &name, node)
    }

    def getdelta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyObject> {
        let store = self.store(py);
        store.get_delta_py(py, &name, node)
    }

    def getdeltachain(&self, name: PyPath, node: &PyBytes) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_delta_chain_py(py, &name, node)
    }

    def getmeta(&self, name: PyPath, node: &PyBytes) -> PyResult<PyDict> {
        let store = self.store(py);
        store.get_meta_py(py, &name, node)
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        let store = self.store(py);
        store.get_missing_py(py, &mut keys.iter(py)?)
    }

    def add(&self, name: PyPath, node: &PyBytes, deltabasenode: &PyBytes, delta: &PyBytes, metadata: Option<PyDict> = None) -> PyResult<PyObject> {
        let store = self.store(py);
        store.add_py(py, &name, node, deltabasenode, delta, metadata)
    }

    def flush(&self) -> PyResult<Option<PyPath>> {
        let store = self.store(py);
        store.flush_py(py)
    }

    def prefetch(&self, keys: PyList) -> PyResult<PyObject> {
        let store = self.store(py);
        store.prefetch_py(py, keys)
    }
});

py_class!(class metadatastore |py| {
    data store: MetadataStore;

    def __new__(_cls, path: Option<PyPath>, config: config, remote: pyremotestore) -> PyResult<metadatastore> {
        let remotestore = remote.into_inner(py);
        let config = config.get_cfg(py);

        let mut builder = MetadataStoreBuilder::new(&config).remotestore(Box::new(remotestore));

        builder = if let Some(path) = path {
            builder.local_path(path)
        } else {
            builder.no_local_store()
        };

        let metadatastore = builder.build().map_pyerr(py)?;
        metadatastore::create_instance(py, metadatastore)
    }

    def getnodeinfo(&self, name: PyPath, node: &PyBytes) -> PyResult<PyTuple> {
        self.store(py).get_node_info_py(py, &name, node)
    }

    def getmissing(&self, keys: &PyObject) -> PyResult<PyList> {
        self.store(py).get_missing_py(py, &mut keys.iter(py)?)
    }

    def add(&self, name: PyPath, node: &PyBytes, p1: &PyBytes, p2: &PyBytes, linknode: &PyBytes, copyfrom: Option<PyPath>) -> PyResult<PyObject> {
        let store = self.store(py);
        store.add_py(py, &name, node, p1, p2, linknode, copyfrom.as_ref())
    }

    def flush(&self) -> PyResult<Option<PyPath>> {
        let store = self.store(py);
        store.flush_py(py)
    }

    def prefetch(&self, keys: PyList) -> PyResult<PyObject> {
        let store = self.store(py);
        store.prefetch_py(py, keys)
    }
});
