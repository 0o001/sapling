/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "HgBackingStore.h"

#include <folly/Synchronized.h>
#include <folly/ThreadLocal.h>
#include <folly/Try.h>
#include <folly/executors/CPUThreadPoolExecutor.h>
#include <folly/executors/GlobalExecutor.h>
#include <folly/executors/task_queue/UnboundedBlockingQueue.h>
#include <folly/executors/thread_factory/NamedThreadFactory.h>
#include <folly/futures/Future.h>
#include <folly/logging/xlog.h>
#include "eden/fs/config/ReloadableConfig.h"
#include "eden/fs/eden-config.h"
#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/store/LocalStore.h"
#include "eden/fs/store/SerializedBlobMetadata.h"
#include "eden/fs/store/StoreResult.h"
#include "eden/fs/store/hg/HgImportPyError.h"
#include "eden/fs/store/hg/HgImporter.h"
#include "eden/fs/store/hg/HgProxyHash.h"
#include "eden/fs/store/mononoke/MononokeHttpBackingStore.h"
#include "eden/fs/utils/LazyInitialize.h"
#include "eden/fs/utils/SSLContext.h"
#include "eden/fs/utils/ServiceAddress.h"
#include "eden/fs/utils/UnboundedQueueExecutor.h"

#if EDEN_HAVE_HG_TREEMANIFEST
#include "edenscm/hgext/extlib/cstore/uniondatapackstore.h" // @manual=//scm/hg:datapack
#include "edenscm/hgext/extlib/ctreemanifest/treemanifest.h" // @manual=//scm/hg:datapack
#ifndef EDEN_WIN_NO_RUST_DATAPACK
#include "scm/hg/lib/configparser/ConfigParser.h"
#endif
#endif // EDEN_HAVE_HG_TREEMANIFEST

#ifndef EDEN_WIN_NOMONONOKE
#include "eden/fs/store/mononoke/MononokeHttpBackingStore.h"
#include "eden/fs/store/mononoke/MononokeThriftBackingStore.h"
#endif

#ifdef EDEN_HAVE_CURL
#include "eden/fs/store/mononoke/MononokeCurlBackingStore.h"
#endif

using folly::ByteRange;
using folly::Future;
using folly::IOBuf;
using folly::makeFuture;
using folly::StringPiece;
using std::make_unique;
using std::unique_ptr;
using KeySpace = facebook::eden::LocalStore::KeySpace;

DEFINE_int32(
    num_hg_import_threads,
    // Why 8? 1 is materially slower but 24 is no better than 4 in a simple
    // microbenchmark that touches all files.  8 is better than 4 in the case
    // that we need to fetch a bunch from the network.
    // See benchmarks in the doc linked from D5067763.
    // Note that this number would benefit from occasional revisiting.
    8,
    "the number of hg import threads per repo");
DEFINE_bool(
    hg_fetch_missing_trees,
    true,
    "Set this parameter to \"no\" to disable fetching missing treemanifest "
    "trees from the remote mercurial server.  This is generally only useful "
    "for testing/debugging purposes");
DEFINE_bool(
    use_hg_tree_manifest,
    // treemanifest imports are disabled by default for now.
    // We currently cannot access treemanifest data for pending transactions
    // when mercurial invokes dirstate.setparents(), and this breaks
    // many workflows.
    true,
    "Import mercurial trees using treemanifest in supported repositories.");
DEFINE_bool(
    allow_flatmanifest_fallback,
    true,
    "In mercurial repositories that support treemanifest, allow importing "
    "commit information using flatmanifest if tree if an error occurs trying "
    "to get treemanifest data.");
DEFINE_int32(
    mononoke_timeout,
    2000, // msec
    "[unit: ms] Timeout for Mononoke requests");

namespace facebook {
namespace eden {

namespace {
// Thread local HgImporter. This is only initialized on HgImporter threads.
static folly::ThreadLocalPtr<Importer> threadLocalImporter;

/**
 * Checks that the thread local HgImporter is present and returns it.
 */
Importer& getThreadLocalImporter() {
  if (!threadLocalImporter) {
    throw std::logic_error(
        "Attempting to get HgImporter from non-HgImporter thread");
  }
  return *threadLocalImporter;
}

/**
 * Thread factory that sets thread name and initializes a thread local
 * HgImporter.
 */
class HgImporterThreadFactory : public folly::ThreadFactory {
 public:
  HgImporterThreadFactory(
      AbsolutePathPiece repository,
      LocalStore* localStore,
      std::shared_ptr<EdenStats> stats)
      : delegate_("HgImporter"),
        repository_(repository),
        localStore_(localStore),
        stats_(std::move(stats)) {}

  std::thread newThread(folly::Func&& func) override {
    return delegate_.newThread([this, func = std::move(func)]() mutable {
      threadLocalImporter.reset(
          new HgImporterManager(repository_, localStore_, stats_));
      func();
    });
  }

 private:
  folly::NamedThreadFactory delegate_;
  AbsolutePath repository_;
  LocalStore* localStore_;
  std::shared_ptr<EdenStats> stats_;
};

/**
 * An inline executor that, while it exists, keeps a thread-local HgImporter
 * instance.
 */
class HgImporterTestExecutor : public folly::InlineExecutor {
 public:
  explicit HgImporterTestExecutor(Importer* importer) {
    threadLocalImporter.reset(importer);
  }

  ~HgImporterTestExecutor() override {
    threadLocalImporter.release();
  }
};

#if EDEN_HAVE_HG_TREEMANIFEST
// A helper function to avoid repeating noisy casts/conversions when
// loading data from a UnionDatapackStore instance.
ConstantStringRef unionStoreGet(
    UnionDatapackStore& unionStore,
    StringPiece name,
    const Hash& id) {
  return unionStore.get(
      Key(name.data(),
          name.size(),
          (const char*)id.getBytes().data(),
          id.getBytes().size()));
}

// A helper function to avoid repeating noisy casts/conversions when
// loading data from a UnionDatapackStore instance.  This variant will
// ask the store to rescan and look for changed packs if it encounters
// a missing key.
ConstantStringRef unionStoreGetWithRefresh(
    UnionDatapackStore& unionStore,
    StringPiece name,
    const Hash& id) {
  try {
    return unionStoreGet(unionStore, name, id);
  } catch (const MissingKeyError& ex) {
    unionStore.markForRefresh();
    return unionStoreGet(unionStore, name, id);
  }
}

std::unique_ptr<Blob> getBlobFromUnionStore(
    UnionDatapackStore& unionStore,
    const Hash& id,
    const HgProxyHash& hgInfo) {
  try {
    auto content = unionStoreGetWithRefresh(
        unionStore, hgInfo.path().stringPiece(), hgInfo.revHash());
    if (content.content()) {
      XLOG(DBG5) << "loaded datapack for " << hgInfo.path() << " hash "
                 << hgInfo.revHash() << ", it has size " << content.size();
      return make_unique<Blob>(
          id, IOBuf(IOBuf::CopyBufferOp{}, content.content(), content.size()));
    }
  } catch (const MissingKeyError&) {
    // Data for this blob was not present locally.
  }
  return nullptr;
}

#ifndef EDEN_WIN_NO_RUST_DATAPACK
std::unique_ptr<Blob> getBlobFromDataPackUnion(
    DataPackUnion& store,
    const Hash& id,
    const HgProxyHash& hgInfo) {
  try {
    auto content =
        store.get(hgInfo.path().stringPiece(), hgInfo.revHash().getBytes());
    if (content) {
      auto bytes = content->bytes();
      return make_unique<Blob>(
          id, IOBuf(IOBuf::CopyBufferOp{}, bytes.data(), bytes.size()));
    }
    // If we get here, it was a KeyError, meaning that the data wasn't
    // present in the hgcache, rather than a more terminal problems such
    // as an IOError of some kind.
    // Regardless, we'll return nullptr and fallback to other sources.
  } catch (const DataPackUnionGetError& exc) {
    XLOG(ERR) << "Error getting " << hgInfo.path() << " " << hgInfo.revHash()
              << " from dataPackStore_: " << exc.what()
              << ", will fall back to other methods";
  }
  return nullptr;
}
#endif
#endif
} // namespace

HgBackingStore::HgBackingStore(
    AbsolutePathPiece repository,
    LocalStore* localStore,
    UnboundedQueueExecutor* serverThreadPool,
    std::shared_ptr<ReloadableConfig> config,
    std::shared_ptr<EdenStats> stats)
    : localStore_(localStore),
      importThreadPool_(make_unique<folly::CPUThreadPoolExecutor>(
          FLAGS_num_hg_import_threads,
          /* Eden performance will degrade when, for example, a status operation
           * causes a large number of import requests to be scheduled before a
           * lightweight operation needs to check the RocksDB cache. In that
           * case, the RocksDB threads can end up all busy inserting work into
           * the importer queue, preventing future requests that would hit cache
           * from succeeding.
           *
           * Thus, make the import queue unbounded.
           *
           * In the long term, we'll want a more comprehensive approach to
           * bounding the parallelism of scheduled work.
           */
          make_unique<folly::UnboundedBlockingQueue<
              folly::CPUThreadPoolExecutor::CPUTask>>(),
          std::make_shared<HgImporterThreadFactory>(
              repository,
              localStore,
              stats))),
      config_(config),
      serverThreadPool_(serverThreadPool) {
#if EDEN_HAVE_HG_TREEMANIFEST
#ifndef EDEN_WIN_NO_RUST_DATAPACK
  initializeDatapackImport(repository);
#endif
  HgImporter importer(repository, localStore, std::move(stats));
  const auto& options = importer.getOptions();
  initializeTreeManifestImport(options, repository);
  repoName_ = options.repoName;
#endif // EDEN_HAVE_HG_TREEMANIFEST
}

/**
 * Create an HgBackingStore suitable for use in unit tests. It uses an inline
 * executor to process loaded objects rather than the thread pools used in
 * production Eden.
 */
HgBackingStore::HgBackingStore(Importer* importer, LocalStore* localStore)
    : localStore_{localStore},
      importThreadPool_{std::make_unique<HgImporterTestExecutor>(importer)},
      serverThreadPool_{importThreadPool_.get()} {}

HgBackingStore::~HgBackingStore() {}

#ifndef EDEN_WIN_NO_RUST_DATAPACK
namespace {
folly::Synchronized<DataPackUnion> makeUnionStore(
    AbsolutePathPiece repository,
    folly::StringPiece repoName,
    AbsolutePathPiece cachePath,
    RelativePathPiece subdir) {
  std::vector<AbsolutePath> paths;

  paths.emplace_back(repository + ".hg/store"_relpath + subdir);
  paths.emplace_back(cachePath + RelativePathPiece{repoName} + subdir);

  std::vector<const char*> cStrings;
  for (auto& path : paths) {
    cStrings.emplace_back(path.c_str());
  }
  return folly::Synchronized<DataPackUnion>(
      DataPackUnion(cStrings.data(), cStrings.size()));
}
} // namespace

void HgBackingStore::initializeDatapackImport(AbsolutePathPiece repository) {
  HgRcConfigSet config;

  auto repoConfigPath = repository + ".hg/hgrc"_relpath;

  try {
    config.loadSystem();
    config.loadUser();
    config.loadPath(repoConfigPath.c_str());
  } catch (const HgRcConfigError& exc) {
    XLOG(ERR)
        << "Disabling loading blobs from hgcache: Error(s) while loading '"
        << repoConfigPath << "': " << exc.what();
    return;
  }

  auto maybeRepoName = config.get("remotefilelog", "reponame");
  auto maybeCachePath = config.get("remotefilelog", "cachepath");

  if (maybeRepoName.hasValue() && maybeCachePath.hasValue()) {
    folly::StringPiece repoName{maybeRepoName.value().bytes()};

    std::optional<StringPiece> homeDir = config_
        ? std::make_optional(
              config_->getEdenConfig()->getUserHomePath().stringPiece())
        : std::nullopt;
    auto cachePath =
        expandUser(StringPiece{maybeCachePath.value().bytes()}, homeDir);

    dataPackStore_ =
        makeUnionStore(repository, repoName, cachePath, "packs"_relpath);
    // TODO: create a treePackStore here with `packs/manifests` as the subdir.
    // That depends on some future work to port the manifest code from C++
    // to Rust.
  } else {
    XLOG(DBG2)
        << "Disabling loading blobs from hgcache: remotefilelog.reponame "
           "and/or remotefilelog.cachepath are not configured";
  }
}
#endif

void HgBackingStore::initializeTreeManifestImport(
    const ImporterOptions& options,
    AbsolutePathPiece repoPath) {
#if EDEN_HAVE_HG_TREEMANIFEST
  if (!FLAGS_use_hg_tree_manifest) {
    XLOG(DBG2) << "treemanifest import disabled via command line flags "
                  "for repository "
               << repoPath;
    return;
  }
  if (options.treeManifestPackPaths.empty()) {
    XLOG(DBG2) << "treemanifest import not supported in repository "
               << repoPath;
    return;
  }

  std::vector<DataStore*> storePtrs;
  for (const auto& path : options.treeManifestPackPaths) {
    XLOG(DBG5) << "treemanifest pack path: " << path;
    // Create a new DatapackStore for path.  Note that we enable removing
    // dead pack files.  This is only guaranteed to be safe so long as we copy
    // the relevant data out of the datapack objects before we issue a
    // subsequent call into the unionStore_.
    dataPackStores_.emplace_back(std::make_unique<DatapackStore>(path, true));
    storePtrs.emplace_back(dataPackStores_.back().get());
  }

  unionStore_ = std::make_unique<folly::Synchronized<UnionDatapackStore>>(
      folly::in_place, storePtrs);
  XLOG(DBG2) << "treemanifest import enabled in repository " << repoPath;
#endif // EDEN_HAVE_HG_TREEMANIFEST
}

std::unique_ptr<ServiceAddress> HgBackingStore::getMononokeServiceAddress() {
  auto edenConfig = config_->getEdenConfig();
  auto hostname = edenConfig->getMononokeHostName();

  if (hostname) {
    auto port = edenConfig->getMononokePort();
    XLOG(DBG2) << "Using " << *hostname << ":" << port << " for Mononoke";
    return std::make_unique<ServiceAddress>(*hostname, port);
  }

  const auto& tier = edenConfig->getMononokeTierName();
  XLOG(DBG2) << "Using SMC tier " << tier << " for Mononoke";
  return std::make_unique<ServiceAddress>(tier);
}

#ifndef EDEN_WIN_NOMONONOKE
std::unique_ptr<MononokeHttpBackingStore>
HgBackingStore::initializeHttpMononokeBackingStore() {
  auto edenConfig = config_->getEdenConfig();
  std::shared_ptr<folly::SSLContext> sslContext;

  try {
    auto clientCertificate = edenConfig->getClientCertificate();
    sslContext = buildSSLContext(clientCertificate);
  } catch (std::runtime_error& ex) {
    XLOG(WARN) << "mononoke is disabled because of build failure when "
                  "creating SSLContext: "
               << ex.what();
    return nullptr;
  }

  return std::make_unique<MononokeHttpBackingStore>(
      getMononokeServiceAddress(),
      repoName_,
      std::chrono::milliseconds(FLAGS_mononoke_timeout),
      folly::getIOExecutor().get(),
      sslContext);
}

std::unique_ptr<MononokeThriftBackingStore>
HgBackingStore::initializeThriftMononokeBackingStore() {
  auto edenConfig = config_->getEdenConfig();
  auto tierName = edenConfig->getMononokeTierName();
  auto executor = folly::getIOExecutor();

  XLOG(DBG2) << "Initializing thrift Mononoke backing store for repository "
             << repoName_ << ", using tier " << tierName;
  return std::make_unique<MononokeThriftBackingStore>(
      tierName, repoName_, executor);
}
#endif

#if defined(EDEN_HAVE_CURL) && EDEN_HAVE_HG_TREEMANIFEST
std::unique_ptr<MononokeCurlBackingStore>
HgBackingStore::initializeCurlMononokeBackingStore() {
  auto edenConfig = config_->getEdenConfig();
  auto clientCertificate = edenConfig->getClientCertificate();

  if (!clientCertificate) {
    XLOG(WARN)
        << "Mononoke is disabled because no client certificate is provided";
    return nullptr;
  }

  return std::make_unique<MononokeCurlBackingStore>(
      getMononokeServiceAddress(),
      AbsolutePath(folly::to<std::string>(*clientCertificate)),
      repoName_,
      std::chrono::milliseconds(FLAGS_mononoke_timeout),
      folly::getCPUExecutor());
}
#endif

std::unique_ptr<BackingStore> HgBackingStore::initializeMononoke() {
#if EDEN_HAVE_HG_TREEMANIFEST
  const auto& connectionType =
      config_->getEdenConfig()->getMononokeConnectionType();
#ifndef EDEN_WIN_NOMONONOKE
  if (connectionType == "http") {
    return initializeHttpMononokeBackingStore();
  } else if (connectionType == "thrift") {
    return initializeThriftMononokeBackingStore();
  } else if (connectionType == "curl") {
#ifdef EDEN_HAVE_CURL
    return initializeCurlMononokeBackingStore();
#else // EDEN_HAVE_CURL
    XLOG(WARN)
        << "User specified Mononoke connection type as cURL, but eden is built "
           "without cURL";
#endif // EDEN_HAVE_CURL
  } else {
    XLOG(WARN) << "got unexpected value for `mononoke:connection-type`: "
               << connectionType;
  }
#elif defined(EDEN_HAVE_CURL) // EDEN_WIN_NOMONONOKE
  return initializeCurlMononokeBackingStore();
#endif // EDEN_WIN_NOMONONOKE
#endif // EDEN_HAVE_HG_TREEMANIFEST
  return nullptr;
}

Future<unique_ptr<Tree>> HgBackingStore::getTree(const Hash& id) {
#if EDEN_HAVE_HG_TREEMANIFEST
  HgProxyHash pathInfo(localStore_, id, "importTree");
  std::shared_ptr<LocalStore::WriteBatch> writeBatch(localStore_->beginWrite());
  auto fut = importTreeImpl(
      pathInfo.revHash(), // this is really the manifest node
      id,
      pathInfo.path(),
      writeBatch);
  return std::move(fut).thenValue([batch = std::move(writeBatch)](auto tree) {
    batch->flush();
    return tree;
  });
#else
  return Future<unique_ptr<Tree>>(folly::make_exception_wrapper<
                                  std::domain_error>(folly::to<std::string>(
      "requested to import subtree ",
      id.toString(),
      " but flatmanifest import should have already imported all subtrees")));
#endif
}

#if EDEN_HAVE_HG_TREEMANIFEST
Future<unique_ptr<Tree>> HgBackingStore::importTreeImpl(
    const Hash& manifestNode,
    const Hash& edenTreeID,
    RelativePathPiece path,
    std::shared_ptr<LocalStore::WriteBatch> writeBatch) {
  XLOG(DBG6) << "importing tree " << edenTreeID << ": hg manifest "
             << manifestNode << " for path \"" << path << "\"";

  // Explicitly check for the null ID on the root directory.
  // This isn't actually present in the mercurial data store; it has to be
  // handled specially in the code.
  if (path.empty() && manifestNode == kZeroHash) {
    auto tree = make_unique<Tree>(std::vector<TreeEntry>{}, edenTreeID);
    auto serialized = LocalStore::serializeTree(tree.get());
    writeBatch->put(
        KeySpace::TreeFamily, edenTreeID, serialized.second.coalesce());
    return tree;
  }

  auto mononoke = getMononoke();
  if (mononoke) {
    // ask Mononoke API Server first because it has more metadata available
    // than we'd get from a local treepack.  Getting that data from mononoke
    // can save us from materializing so many file contents later to compute
    // size and hash information.
    XLOG(DBG4) << "importing tree \"" << manifestNode << "\" from mononoke";

    RelativePath ownedPath(path);
    return mononoke->getTree(manifestNode)
        .via(serverThreadPool_)
        .thenTry([edenTreeID, ownedPath, writeBatch](
                     auto mononokeTreeTry) mutable {
          auto& mononokeTree = mononokeTreeTry.value();
          std::vector<TreeEntry> entries;

          for (const auto& entry : mononokeTree->getTreeEntries()) {
            auto blobHash = entry.getHash();
            auto entryName = entry.getName();
            auto proxyHash = HgProxyHash::store(
                ownedPath + entryName, blobHash, writeBatch.get());

            entries.emplace_back(
                proxyHash, entryName.stringPiece(), entry.getType());

            if (entry.getContentSha1() && entry.getSize()) {
              BlobMetadata metadata{*entry.getContentSha1(), *entry.getSize()};

              SerializedBlobMetadata metadataBytes(metadata);
              auto hashSlice = proxyHash.getBytes();
              writeBatch->put(
                  KeySpace::BlobMetaDataFamily,
                  hashSlice,
                  metadataBytes.slice());
            }
          }

          auto tree = make_unique<Tree>(std::move(entries), edenTreeID);
          auto serialized = LocalStore::serializeTree(tree.get());
          writeBatch->put(
              KeySpace::TreeFamily, edenTreeID, serialized.second.coalesce());
          return makeFuture(std::move(tree));
        })
        .thenError([this, manifestNode, edenTreeID, ownedPath, writeBatch](
                       const folly::exception_wrapper& ex) mutable {
          XLOG(WARN) << "got exception from MononokeHttpBackingStore: "
                     << ex.what();
          return fetchTreeFromHgCacheOrImporter(
              manifestNode,
              edenTreeID,
              std::move(ownedPath),
              std::move(writeBatch));
        });
  }

  return fetchTreeFromHgCacheOrImporter(
      manifestNode, edenTreeID, path.copy(), writeBatch);
}

folly::Future<std::unique_ptr<Tree>>
HgBackingStore::fetchTreeFromHgCacheOrImporter(
    Hash manifestNode,
    Hash edenTreeID,
    RelativePath path,
    std::shared_ptr<LocalStore::WriteBatch> writeBatch) {
  try {
    auto content = unionStoreGetWithRefresh(
        *unionStore_->wlock(), path.stringPiece(), manifestNode);
    return folly::makeFuture(
        processTree(content, manifestNode, edenTreeID, path, writeBatch.get()));
  } catch (const MissingKeyError&) {
    // Data for this tree was not present locally.
    // Fall through and fetch the data from the server below.
    if (!FLAGS_hg_fetch_missing_trees) {
      auto ew = folly::exception_wrapper(std::current_exception());
      return folly::makeFuture<unique_ptr<Tree>>(ew);
    }
    return fetchTreeFromImporter(
        manifestNode, edenTreeID, std::move(path), std::move(writeBatch));
  }
}

std::shared_ptr<BackingStore> HgBackingStore::getMononoke() {
  // config_ might be uninitialized (e.g. testing).
  if (!config_ || repoName_.empty()) {
    return nullptr;
  }

  // Check to see if the user has disabled mononoke since starting the server.
  auto useMononoke = config_->getEdenConfig()->getUseMononoke();

  return lazyInitialize<BackingStore>(
      useMononoke, mononoke_, [this]() { return initializeMononoke(); });
}

folly::Future<std::unique_ptr<Tree>> HgBackingStore::fetchTreeFromImporter(
    Hash manifestNode,
    Hash edenTreeID,
    RelativePath path,
    std::shared_ptr<LocalStore::WriteBatch> writeBatch) {
  auto fut =
      folly::via(
          importThreadPool_.get(),
          [path, manifestNode] {
            return getThreadLocalImporter().fetchTree(path, manifestNode);
          })
          .via(serverThreadPool_);
  return std::move(fut).thenTry(
      [this,
       ownedPath = std::move(path),
       node = std::move(manifestNode),
       treeID = std::move(edenTreeID),
       batch = std::move(writeBatch)](folly::Try<folly::Unit> val) {
        try {
          val.value();
          // Now try loading it again
          unionStore_->wlock()->markForRefresh();
          auto content = unionStoreGet(
              *unionStore_->wlock(), ownedPath.stringPiece(), node);
          return processTree(content, node, treeID, ownedPath, batch.get());
        } catch (const HgImportPyError& ex) {
          if (FLAGS_allow_flatmanifest_fallback) {
            // For now translate any error thrown into a MissingKeyError,
            // so that our caller will retry this tree import using
            // flatmanifest import if possible.
            //
            // The mercurial code can throw a wide variety of errors here
            // that all effectively mean mean it couldn't fetch the tree
            // data.
            //
            // We most commonly expect to get a MissingNodesError if the
            // remote server does not know about these trees (for instance
            // if they are only available locally, but simply only have
            // flatmanifest information rather than treemanifest info).
            //
            // However we can also get lots of other errors: no remote
            // server configured, remote repository does not exist, remote
            // repository does not support fetching tree info, etc.
            throw MissingKeyError(ex.what());
          } else {
            throw;
          }
        }
      });
}

std::unique_ptr<Tree> HgBackingStore::processTree(
    ConstantStringRef& content,
    const Hash& manifestNode,
    const Hash& edenTreeID,
    RelativePathPiece path,
    LocalStore::WriteBatch* writeBatch) {
  if (!content.content()) {
    // This generally shouldn't happen: the UnionDatapackStore throws on
    // error instead of returning null.  We're checking simply due to an
    // abundance of caution.
    throw std::domain_error(folly::to<std::string>(
        "HgBackingStore::importTree received null tree from mercurial store for ",
        path,
        ", ID ",
        manifestNode.toString()));
  }
  Manifest manifest(
      content, reinterpret_cast<const char*>(manifestNode.getBytes().data()));
  std::vector<TreeEntry> entries;

  auto iter = manifest.getIterator();
  while (!iter.isfinished()) {
    auto* entry = iter.currentvalue();

    // The node is the hex string representation of the hash, but
    // it is not NUL terminated!
    StringPiece node(entry->get_node(), 40);
    Hash entryHash(node);

    StringPiece entryName(entry->filename, entry->filenamelen);

    TreeEntryType fileType;

    StringPiece entryFlag;
    if (entry->flag) {
      // entry->flag is a char* but is unfortunately not nul terminated.
      // All known flag values are currently only a single character, and
      // there are never any multi-character flags.
      entryFlag.assign(entry->flag, entry->flag + 1);
    }

    XLOG(DBG9) << "tree: " << manifestNode << " " << entryName
               << " node: " << node << " flag: " << entryFlag;

    if (entry->isdirectory()) {
      fileType = TreeEntryType::TREE;
    } else if (entry->flag) {
      switch (*entry->flag) {
        case 'x':
          fileType = TreeEntryType::EXECUTABLE_FILE;
          break;
        case 'l':
          fileType = TreeEntryType::SYMLINK;
          break;
        default:
          throw std::runtime_error(folly::to<std::string>(
              "unsupported file flags for ",
              path,
              "/",
              entryName,
              ": ",
              entryFlag));
      }
    } else {
      fileType = TreeEntryType::REGULAR_FILE;
    }

    auto proxyHash = HgProxyHash::store(
        path + RelativePathPiece(entryName), entryHash, writeBatch);

    entries.emplace_back(proxyHash, entryName, fileType);

    iter.next();
  }

  auto tree = make_unique<Tree>(std::move(entries), edenTreeID);
  auto serialized = LocalStore::serializeTree(tree.get());
  writeBatch->put(
      KeySpace::TreeFamily, edenTreeID, serialized.second.coalesce());
  return tree;
}

folly::Future<Hash> HgBackingStore::importTreeManifest(const Hash& commitId) {
  return folly::via(
             importThreadPool_.get(),
             [commitId] {
               return getThreadLocalImporter().resolveManifestNode(
                   commitId.toString());
             })
      .via(serverThreadPool_)
      .thenValue([this, commitId](auto manifestNode) {
        XLOG(DBG2) << "revision " << commitId.toString()
                   << " has manifest node " << manifestNode;
        // Record that we are at the root for this node
        RelativePathPiece path{};
        auto proxyInfo = HgProxyHash::prepareToStore(path, manifestNode);
        std::shared_ptr<LocalStore::WriteBatch> writeBatch(
            localStore_->beginWrite());
        auto futTree =
            importTreeImpl(manifestNode, proxyInfo.first, path, writeBatch);
        return std::move(futTree).thenValue(
            [batch = std::move(writeBatch),
             info = std::move(proxyInfo)](auto tree) {
              // Only write the proxy hash value for this once we've imported
              // the root.
              HgProxyHash::store(info, batch.get());
              batch->flush();

              return tree->getHash();
            });
      });
}
#endif // EDEN_HAVE_HG_TREEMANIFEST

Future<unique_ptr<Blob>> HgBackingStore::getBlob(const Hash& id) {
  // Look up the mercurial path and file revision hash,
  // which we need to import the data from mercurial
  HgProxyHash hgInfo(localStore_, id, "importFileContents");

#if EDEN_HAVE_HG_TREEMANIFEST
#ifndef EDEN_WIN_NO_RUST_DATAPACK
  if (useDatapackGetBlob_ && dataPackStore_) {
    auto content =
        getBlobFromDataPackUnion(*dataPackStore_.value().wlock(), id, hgInfo);
    if (content) {
      return makeFuture(std::move(content));
    }
  } else
#endif
      // Prefer using the above rust implementation over the C++ implementation
      if (useDatapackGetBlob_ && unionStore_) {
    auto content = getBlobFromUnionStore(*unionStore_->wlock(), id, hgInfo);
    if (content) {
      return makeFuture(std::move(content));
    }
  }

  auto mononoke = getMononoke();
  if (mononoke) {
    XLOG(DBG5) << "requesting file contents of '" << hgInfo.path() << "', "
               << hgInfo.revHash().toString() << " from mononoke";
    auto revHashCopy = hgInfo.revHash();
    return mononoke->getBlob(revHashCopy)
        .thenError([this,
                    id,
                    path = hgInfo.path().copy(),
                    revHash = revHashCopy](const folly::exception_wrapper& ex) {
          XLOG(ERR) << "Error while fetching file contents of '" << path
                    << "', " << revHash.toString()
                    << " from mononoke: " << ex.what()
                    << ", fall back to import helper.";
          return folly::via(
                     importThreadPool_.get(),
                     [id] {
                       return getThreadLocalImporter().importFileContents(id);
                     })
              // Ensure that the control moves back to the main thread pool
              // to process the caller-attached .then routine.
              .via(serverThreadPool_);
        });
  }
#endif // EDEN_HAVE_HG_TREEMANIFEST

  return folly::via(
             importThreadPool_.get(),
             [id] { return getThreadLocalImporter().importFileContents(id); })
      // Ensure that the control moves back to the main thread pool
      // to process the caller-attached .then routine.
      .via(serverThreadPool_);
}

folly::Future<folly::Unit> HgBackingStore::prefetchBlobs(
    const std::vector<Hash>& ids) const {
  return HgProxyHash::getBatch(localStore_, ids)
      .via(importThreadPool_.get())
      .thenValue([](std::vector<std::pair<RelativePath, Hash>>&& hgPathHashes) {
        return getThreadLocalImporter().prefetchFiles(hgPathHashes);
      })
      .via(serverThreadPool_);
}

Future<unique_ptr<Tree>> HgBackingStore::getTreeForCommit(
    const Hash& commitID) {
  // Ensure that the control moves back to the main thread pool
  // to process the caller-attached .then routine.
  return getTreeForCommitImpl(commitID).via(serverThreadPool_);
}

folly::Future<unique_ptr<Tree>> HgBackingStore::getTreeForCommitImpl(
    Hash commitID) {
  return localStore_
      ->getFuture(KeySpace::HgCommitToTreeFamily, commitID.getBytes())
      .thenValue(
          [this,
           commitID](StoreResult result) -> folly::Future<unique_ptr<Tree>> {
            if (!result.isValid()) {
              return importTreeForCommit(commitID);
            }

            auto rootTreeHash = Hash{result.bytes()};
            XLOG(DBG5) << "found existing tree " << rootTreeHash.toString()
                       << " for mercurial commit " << commitID.toString();

            return localStore_->getTree(rootTreeHash)
                .thenValue(
                    [this, rootTreeHash, commitID](std::unique_ptr<Tree> tree)
                        -> folly::Future<unique_ptr<Tree>> {
                      if (tree) {
                        return std::move(tree);
                      }

                      // No corresponding tree for this commit ID! Must
                      // re-import. This could happen if RocksDB is corrupted
                      // in some way or deleting entries races with
                      // population.
                      XLOG(WARN) << "No corresponding tree " << rootTreeHash
                                 << " for commit " << commitID
                                 << "; will import again";
                      return importTreeForCommit(commitID);
                    });
          });
}

folly::Future<Hash> HgBackingStore::importManifest(Hash commitId) {
#if EDEN_HAVE_HG_TREEMANIFEST
  if (unionStore_) {
    auto hash = importTreeManifest(commitId);
    if (FLAGS_allow_flatmanifest_fallback) {
      return std::move(hash).thenError(
          folly::tag_t<MissingKeyError>{},
          [this, commitId](const MissingKeyError&) {
            // We don't have a tree manifest available for the target rev,
            // so let's fall through to the full flat manifest importer.
            XLOG(INFO) << "no treemanifest data available for revision "
                       << commitId.toString()
                       << ": falling back to slower flatmanifest import";
            return importFlatManifest(commitId);
          });
    }
    return hash;
  }
#endif // EDEN_HAVE_HG_TREEMANIFEST
  return importFlatManifest(commitId);
}

folly::Future<Hash> HgBackingStore::importFlatManifest(Hash commitId) {
  return folly::via(
             importThreadPool_.get(),
             [commitId] {
               return getThreadLocalImporter().importFlatManifest(
                   commitId.toString());
             })
      .via(serverThreadPool_);
}

folly::Future<unique_ptr<Tree>> HgBackingStore::importTreeForCommit(
    Hash commitID) {
  return importManifest(commitID).thenValue(
      [this, commitID](Hash rootTreeHash) {
        XLOG(DBG1) << "imported mercurial commit " << commitID.toString()
                   << " as tree " << rootTreeHash.toString();

        localStore_->put(
            KeySpace::HgCommitToTreeFamily, commitID, rootTreeHash.getBytes());
        return localStore_->getTree(rootTreeHash);
      });
}

} // namespace eden
} // namespace facebook
