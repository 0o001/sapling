/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "HgBackingStore.h"

#include <list>

#include <folly/Synchronized.h>
#include <folly/ThreadLocal.h>
#include <folly/Try.h>
#include <folly/executors/CPUThreadPoolExecutor.h>
#include <folly/executors/GlobalExecutor.h>
#include <folly/executors/task_queue/UnboundedBlockingQueue.h>
#include <folly/executors/thread_factory/NamedThreadFactory.h>
#include <folly/futures/Future.h>
#include <folly/logging/xlog.h>
#include <folly/small_vector.h>
#include <folly/stop_watch.h>

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
#include "eden/fs/telemetry/RequestMetricsScope.h"
#include "eden/fs/utils/Bug.h"
#include "eden/fs/utils/LazyInitialize.h"
#include "eden/fs/utils/SSLContext.h"
#include "eden/fs/utils/ServiceAddress.h"
#include "eden/fs/utils/UnboundedQueueExecutor.h"

#include "edenscm/hgext/extlib/cstore/uniondatapackstore.h" // @manual=//eden/scm:datapack
#include "edenscm/hgext/extlib/ctreemanifest/treemanifest.h" // @manual=//eden/scm:datapack

#ifdef EDEN_HAVE_RUST_DATAPACK
#include "eden/fs/store/hg/HgDatapackStore.h" // @manual
#endif

#if EDEN_HAVE_MONONOKE
#include "eden/fs/store/mononoke/MononokeHttpBackingStore.h" // @manual
#include "eden/fs/store/mononoke/MononokeThriftBackingStore.h" // @manual
#endif

#ifdef EDEN_HAVE_CURL
#include "eden/fs/store/mononoke/MononokeCurlBackingStore.h" // @manual
#endif

using folly::Future;
using folly::IOBuf;
using folly::makeFuture;
using folly::SemiFuture;
using folly::StringPiece;
using std::make_unique;
using std::unique_ptr;

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
DEFINE_int32(
    mononoke_timeout,
    120000, // msec
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
      std::shared_ptr<EdenStats> stats)
      : delegate_("HgImporter"),
        repository_(repository),
        stats_(std::move(stats)) {}

  std::thread newThread(folly::Func&& func) override {
    return delegate_.newThread([this, func = std::move(func)]() mutable {
      threadLocalImporter.reset(new HgImporterManager(repository_, stats_));
      func();
    });
  }

 private:
  folly::NamedThreadFactory delegate_;
  AbsolutePath repository_;
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
  } catch (const MissingKeyError&) {
    unionStore.markForRefresh();
    return unionStoreGet(unionStore, name, id);
  }
}
} // namespace

HgBackingStore::HgBackingStore(
    AbsolutePathPiece repository,
    LocalStore* localStore,
    UnboundedQueueExecutor* serverThreadPool,
    std::shared_ptr<ReloadableConfig> config,
    std::shared_ptr<EdenStats> stats)
    : localStore_(localStore),
      stats_(stats),
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
          std::make_shared<HgImporterThreadFactory>(repository, stats))),
      config_(config),
      serverThreadPool_(serverThreadPool) {
#ifdef EDEN_HAVE_RUST_DATAPACK
  try {
    auto useEdenApi = config->getEdenConfig()->useEdenApi.getValue();
    datapackStore_ =
        std::make_optional<HgDatapackStore>(repository, useEdenApi);
  } catch (const std::runtime_error& ex) {
    XLOG(WARN) << "Rust native store is disabled due to: " << ex.what();
  }
#endif
  HgImporter importer(repository, stats);
  const auto& options = importer.getOptions();
  initializeTreeManifestImport(options, repository);
  repoName_ = options.repoName;
}

/**
 * Create an HgBackingStore suitable for use in unit tests. It uses an inline
 * executor to process loaded objects rather than the thread pools used in
 * production Eden.
 */
HgBackingStore::HgBackingStore(
    AbsolutePathPiece repository,
    HgImporter* importer,
    LocalStore* localStore,
    std::shared_ptr<EdenStats> stats)
    : localStore_{localStore},
      stats_{std::move(stats)},
      importThreadPool_{std::make_unique<HgImporterTestExecutor>(importer)},
      serverThreadPool_{importThreadPool_.get()} {
  const auto& options = importer->getOptions();
  initializeTreeManifestImport(options, repository);
  repoName_ = options.repoName;
}

HgBackingStore::~HgBackingStore() {}

void HgBackingStore::initializeTreeManifestImport(
    const ImporterOptions& options,
    AbsolutePathPiece repoPath) {
  if (options.treeManifestPackPaths.empty()) {
    throw std::runtime_error(folly::to<std::string>(
        "treemanifest import not supported in repository ", repoPath));
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
}

std::unique_ptr<ServiceAddress> HgBackingStore::getMononokeServiceAddress() {
  auto edenConfig = config_->getEdenConfig();
  auto hostname = edenConfig->getMononokeHostName();

  if (hostname) {
    auto port = edenConfig->mononokePort.getValue();
    XLOG(DBG2) << "Using " << *hostname << ":" << port << " for Mononoke";
    return std::make_unique<ServiceAddress>(*hostname, port);
  }

  const auto& tier = edenConfig->mononokeTierName.getValue();
  XLOG(DBG2) << "Using SMC tier " << tier << " for Mononoke";
  return std::make_unique<ServiceAddress>(tier);
}

#if EDEN_HAVE_MONONOKE
std::unique_ptr<MononokeHttpBackingStore>
HgBackingStore::initializeHttpMononokeBackingStore() {
  auto edenConfig = config_->getEdenConfig();
  std::shared_ptr<folly::SSLContext> sslContext;

  try {
    auto clientCertificate = edenConfig->getClientCertificate();
    sslContext = buildSSLContext(clientCertificate);
  } catch (const std::runtime_error& ex) {
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
  auto tierName = edenConfig->mononokeTierName.getValue();

  XLOG(DBG2) << "Initializing thrift Mononoke backing store for repository "
             << repoName_ << ", using tier " << tierName;
  return std::make_unique<MononokeThriftBackingStore>(
      tierName, repoName_, serverThreadPool_);
}
#endif

#if defined(EDEN_HAVE_CURL)
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
#if EDEN_HAVE_MONONOKE
  const auto& connectionType =
      config_->getEdenConfig()->mononokeConnectionType.getValue();
  if (connectionType == "http") {
    return initializeHttpMononokeBackingStore();
  } else if (connectionType == "thrift") {
    return initializeThriftMononokeBackingStore();
  } else if (connectionType == "curl") {
#ifdef EDEN_HAVE_CURL
    return initializeCurlMononokeBackingStore();
#else
    XLOG(WARN)
        << "User specified Mononoke connection type as cURL, but eden is built "
           "without cURL";
#endif
  } else {
    XLOG(WARN) << "got unexpected value for `mononoke:connection-type`: "
               << connectionType;
  }
#elif defined(EDEN_HAVE_CURL)
  return initializeCurlMononokeBackingStore();
#endif
  return nullptr;
}

SemiFuture<unique_ptr<Tree>> HgBackingStore::getTree(
    const Hash& id,
    ImportPriority /* priority */) {
  HgProxyHash pathInfo(localStore_, id, "importTree");
  return importTreeImpl(
      pathInfo.revHash(), // this is really the manifest node
      id,
      pathInfo.path());
}

Future<unique_ptr<Tree>> HgBackingStore::importTreeImpl(
    const Hash& manifestNode,
    const Hash& edenTreeID,
    RelativePathPiece path) {
  XLOG(DBG6) << "importing tree " << edenTreeID << ": hg manifest "
             << manifestNode << " for path \"" << path << "\"";

  // Explicitly check for the null ID on the root directory.
  // This isn't actually present in the mercurial data store; it has to be
  // handled specially in the code.
  if (path.empty() && manifestNode == kZeroHash) {
    auto tree = make_unique<Tree>(std::vector<TreeEntry>{}, edenTreeID);
    return makeFuture(std::move(tree));
  }

  folly::small_vector<Future<unique_ptr<Tree>>, 2> futures;

  folly::stop_watch<std::chrono::milliseconds> watch;
  auto mononoke = getMononoke();
  if (mononoke) {
    // ask Mononoke API Server first because it has more metadata available
    // than we'd get from a local treepack.  Getting that data from mononoke
    // can save us from materializing so many file contents later to compute
    // size and hash information.
    XLOG(DBG4) << "importing tree \"" << manifestNode << "\" from mononoke";

    RelativePath ownedPath(path);
    futures.push_back(
        mononoke->getTree(manifestNode)
            .via(serverThreadPool_)
            .thenValue([stats = stats_,
                        watch,
                        edenTreeID,
                        ownedPath,
                        writeBatch = localStore_->beginWrite()](
                           auto mononokeTree) mutable {
              std::vector<TreeEntry> entries;

              for (const auto& entry : mononokeTree->getTreeEntries()) {
                auto blobHash = entry.getHash();
                auto entryName = entry.getName();
                auto proxyHash = HgProxyHash::store(
                    ownedPath + entryName, blobHash, writeBatch.get());

                entries.emplace_back(
                    proxyHash, entryName.stringPiece(), entry.getType());

                if (entry.getContentSha1() && entry.getSize()) {
                  BlobMetadata metadata{*entry.getContentSha1(),
                                        *entry.getSize()};

                  SerializedBlobMetadata metadataBytes(metadata);
                  auto hashSlice = proxyHash.getBytes();
                  writeBatch->put(
                      KeySpace::BlobMetaDataFamily,
                      hashSlice,
                      metadataBytes.slice());
                }
              }

              writeBatch->flush();

              auto& currentThreadStats =
                  stats->getHgBackingStoreStatsForCurrentThread();
              currentThreadStats.mononokeBackingStoreGetTree.addValue(
                  watch.elapsed().count());

              auto tree = make_unique<Tree>(std::move(entries), edenTreeID);
              return makeFuture(std::move(tree));
            })
            .thenError(
                [manifestNode](const folly::exception_wrapper& ex) mutable {
                  XLOG(WARN)
                      << "got exception from Mononoke backing store: "
                      << ex.what() << " while importing tree " << manifestNode;
                  return folly::makeFuture<unique_ptr<Tree>>(ex);
                }));
  }

  futures.push_back(
      fetchTreeFromHgCacheOrImporter(manifestNode, edenTreeID, path.copy())
          .thenValue([stats = stats_, watch](auto&& result) {
            auto& currentThreadStats =
                stats->getHgBackingStoreStatsForCurrentThread();
            currentThreadStats.hgBackingStoreGetTree.addValue(
                watch.elapsed().count());
            return std::move(result);
          }));

  return folly::collectAnyWithoutException(futures)
      .via(serverThreadPool_)
      .thenValue([](std::pair<size_t, unique_ptr<Tree>>&& result) {
        return std::move(result.second);
      });
}

folly::Future<std::unique_ptr<Tree>>
HgBackingStore::fetchTreeFromHgCacheOrImporter(
    Hash manifestNode,
    Hash edenTreeID,
    RelativePath path) {
  auto writeBatch = localStore_->beginWrite();
  try {
#ifdef EDEN_HAVE_RUST_DATAPACK
    if (config_) {
      auto edenConfig = config_->getEdenConfig();
      if (edenConfig->useHgCache.getValue() && datapackStore_) {
        if (auto tree = datapackStore_->getTree(
                path, manifestNode, edenTreeID, writeBatch.get())) {
          XLOG(DBG4) << "imported tree node=" << manifestNode
                     << " path=" << path << " from Rust hgcache";
          return folly::makeFuture(std::move(tree));
        }
      }
    }
#endif
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
  auto useMononoke = config_->getEdenConfig()->useMononoke.getValue();

  return lazyInitialize<BackingStore>(
      useMononoke, mononoke_, [this]() { return initializeMononoke(); });
}

folly::Future<std::unique_ptr<Tree>> HgBackingStore::fetchTreeFromImporter(
    Hash manifestNode,
    Hash edenTreeID,
    RelativePath path,
    std::shared_ptr<LocalStore::WriteBatch> writeBatch) {
  return folly::via(
             importThreadPool_.get(),
             [path,
              manifestNode,
              stats = stats_,
              &liveImportTreeWatches = liveImportTreeWatches_] {
               Importer& importer = getThreadLocalImporter();
               folly::stop_watch<std::chrono::milliseconds> watch;
               RequestMetricsScope queueTracker{&liveImportTreeWatches};
               importer.fetchTree(path, manifestNode);
               stats->getHgBackingStoreStatsForCurrentThread()
                   .hgBackingStoreImportTree.addValue(watch.elapsed().count());
             })
      .via(serverThreadPool_)
      .thenTry([this,
                ownedPath = std::move(path),
                node = std::move(manifestNode),
                treeID = std::move(edenTreeID),
                batch = std::move(writeBatch)](folly::Try<folly::Unit> val) {
        val.value();
        // Now try loading it again
        unionStore_->wlock()->markForRefresh();
        auto content =
            unionStoreGet(*unionStore_->wlock(), ownedPath.stringPiece(), node);
        return processTree(content, node, treeID, ownedPath, batch.get());
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
  writeBatch->flush();

  return make_unique<Tree>(std::move(entries), edenTreeID);
}

folly::Future<std::unique_ptr<Tree>> HgBackingStore::importTreeManifest(
    const Hash& commitId) {
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
        auto futTree = importTreeImpl(manifestNode, proxyInfo.first, path);
        return std::move(futTree).thenValue(
            [batch = localStore_->beginWrite(),
             info = std::move(proxyInfo)](auto tree) {
              // Only write the proxy hash value for this once we've imported
              // the root.
              HgProxyHash::store(info, batch.get());
              batch->flush();

              return tree;
            });
      });
}

SemiFuture<unique_ptr<Blob>> HgBackingStore::getBlob(
    const Hash& id,
    ImportPriority /* priority */) {
  auto edenConfig = config_->getEdenConfig();

  // Look up the mercurial path and file revision hash,
  // which we need to import the data from mercurial
  HgProxyHash hgInfo(localStore_, id, "importFileContents");

#ifdef EDEN_HAVE_RUST_DATAPACK
  if (edenConfig->useHgCache.getValue() && datapackStore_) {
    if (auto content = datapackStore_->getBlob(id, hgInfo)) {
      XLOG(DBG5) << "importing file contents of '" << hgInfo.path() << "', "
                 << hgInfo.revHash().toString() << " from datapack store";
      return makeFuture(std::move(content));
    }
  }
#endif

  folly::small_vector<SemiFuture<unique_ptr<Blob>>, 2> futures;
  folly::stop_watch<std::chrono::milliseconds> watch;

  auto mononoke = getMononoke();
  if (mononoke) {
    XLOG(DBG5) << "requesting file contents of '" << hgInfo.path() << "', "
               << hgInfo.revHash().toString() << " from mononoke";
    auto revHashCopy = hgInfo.revHash();
    futures.push_back(
        mononoke->getBlob(revHashCopy)
            .deferValue([stats = stats_, watch](auto&& blob) {
              stats->getHgBackingStoreStatsForCurrentThread()
                  .mononokeBackingStoreGetBlob.addValue(
                      watch.elapsed().count());
              return std::move(blob);
            })
            .deferError([path = hgInfo.path().copy(), revHash = revHashCopy](
                            const folly::exception_wrapper& ex) {
              XLOG(WARN) << "Error while fetching file contents of '" << path
                         << "', " << revHash.toString()
                         << " from mononoke: " << ex.what();
              return folly::makeFuture<unique_ptr<Blob>>(ex);
            }));
  }

  futures.push_back(
      getBlobFromHgImporter(hgInfo.path(), hgInfo.revHash())
          .deferValue([stats = stats_,
                       watch](SemiFuture<std::unique_ptr<Blob>>&& blob) {
            stats->getHgBackingStoreStatsForCurrentThread()
                .hgBackingStoreGetBlob.addValue(watch.elapsed().count());
            return std::move(blob);
          }));

  return folly::collectAnyWithoutException(futures).deferValue(
      [](std::pair<size_t, unique_ptr<Blob>>&& result) {
        return std::move(result.second);
      });
}

SemiFuture<std::unique_ptr<Blob>> HgBackingStore::getBlobFromHgImporter(
    const RelativePathPiece& path,
    const Hash& id) {
  return folly::via(
      importThreadPool_.get(),
      [path = path.copy(),
       stats = stats_,
       id,
       &liveImportBlobWatches = liveImportBlobWatches_] {
        Importer& importer = getThreadLocalImporter();
        folly::stop_watch<std::chrono::milliseconds> watch;
        RequestMetricsScope queueTracker{&liveImportBlobWatches};
        auto blob = importer.importFileContents(path, id);
        stats->getHgBackingStoreStatsForCurrentThread()
            .hgBackingStoreImportBlob.addValue(watch.elapsed().count());
        return blob;
      });
}

SemiFuture<folly::Unit> HgBackingStore::prefetchBlobs(
    const std::vector<Hash>& ids) {
  return HgProxyHash::getBatch(localStore_, ids)
      .via(importThreadPool_.get())
      .thenValue([&liveImportPrefetchWatches = liveImportPrefetchWatches_](
                     std::vector<HgProxyHash>&& hgPathHashes) {
        RequestMetricsScope queueTracker{&liveImportPrefetchWatches};
        return getThreadLocalImporter().prefetchFiles(hgPathHashes);
      })
      .via(serverThreadPool_);
}

SemiFuture<unique_ptr<Tree>> HgBackingStore::getTreeForCommit(
    const Hash& commitID) {
  return localStore_
      ->getFuture(KeySpace::HgCommitToTreeFamily, commitID.getBytes())
      .thenValue(
          [this, commitID](
              StoreResult result) -> folly::SemiFuture<unique_ptr<Tree>> {
            if (!result.isValid()) {
              return importTreeForCommit(commitID);
            }

            auto rootTreeHash = Hash{result.bytes()};
            XLOG(DBG5) << "found existing tree " << rootTreeHash.toString()
                       << " for mercurial commit " << commitID.toString();
            return getTreeForRootTreeImpl(commitID, rootTreeHash);
          });
}

folly::SemiFuture<unique_ptr<Tree>> HgBackingStore::getTreeForManifest(
    const Hash& commitID,
    const Hash& manifestID) {
  // Construct the edenTreeID to pass to localStore lookup
  auto rootTreeHash =
      HgProxyHash::prepareToStore(RelativePathPiece{}, manifestID).first;
  return getTreeForRootTreeImpl(commitID, rootTreeHash).via(serverThreadPool_);
}

folly::Future<unique_ptr<Tree>> HgBackingStore::getTreeForRootTreeImpl(
    const Hash& commitID,
    const Hash& rootTreeHash) {
  return localStore_->getTree(rootTreeHash)
      .thenValue(
          [this, rootTreeHash, commitID](
              std::unique_ptr<Tree> tree) -> folly::Future<unique_ptr<Tree>> {
            if (tree) {
              return std::move(tree);
            }

            return localStore_->getTree(rootTreeHash)
                .thenValue(
                    [this, rootTreeHash, commitID](std::unique_ptr<Tree> tree)
                        -> folly::SemiFuture<unique_ptr<Tree>> {
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

folly::SemiFuture<unique_ptr<Tree>> HgBackingStore::importTreeForCommit(
    Hash commitID) {
  return importTreeManifest(commitID).thenValue(
      [this, commitID](std::unique_ptr<Tree> rootTree) {
        XLOG(DBG1) << "imported mercurial commit " << commitID.toString()
                   << " as tree " << rootTree->getHash().toString();

        localStore_->put(
            KeySpace::HgCommitToTreeFamily,
            commitID,
            rootTree->getHash().getBytes());
        return rootTree;
      });
}

std::string HgBackingStore::stringOfHgImportObject(HgImportObject object) {
  switch (object) {
    case HgImportObject::BLOB:
      return "blob";
    case HgImportObject::TREE:
      return "tree";
    case HgImportObject::PREFETCH:
      return "prefetch";
  }
  EDEN_BUG() << "unknown hg import object " << static_cast<int>(object);
}

RequestMetricsScope::LockedRequestWatchList&
HgBackingStore::getLiveImportWatches(HgImportObject object) const {
  switch (object) {
    case HgImportObject::BLOB:
      return liveImportBlobWatches_;
    case HgImportObject::TREE:
      return liveImportTreeWatches_;
    case HgImportObject::PREFETCH:
      return liveImportPrefetchWatches_;
  }
  EDEN_BUG() << "unknown hg import object " << static_cast<int>(object);
}

void HgBackingStore::periodicManagementTask() {
#ifdef EDEN_HAVE_RUST_DATAPACK
  if (datapackStore_) {
    datapackStore_->refresh();
  }
#endif

  if (unionStore_) {
    unionStore_->wlock()->refresh();
  }
}

} // namespace eden
} // namespace facebook
