/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <optional>
#include "eden/fs/service/gen-cpp2/StreamingEdenService.h"
#include "eden/fs/utils/PathFuncs.h"
#include "fb303/BaseService.h"
#ifdef __linux__
#include "eden/fs/service/facebook/EdenFSSmartPlatformServiceEndpoint.h" // @manual
#endif

namespace folly {
template <typename T>
class Future;
}

namespace facebook {
namespace eden {

class Hash;
class EdenMount;
class EdenServer;
class TreeInode;
class ObjectFetchContext;

extern const char* const kServiceName;

/*
 * Handler for the EdenService thrift interface
 */
class EdenServiceHandler : virtual public StreamingEdenServiceSvIf,
                           public fb303::BaseService {
 public:
  explicit EdenServiceHandler(
      std::vector<std::string> originalCommandLine,
      EdenServer* server);

  EdenServiceHandler(EdenServiceHandler const&) = delete;
  EdenServiceHandler& operator=(EdenServiceHandler const&) = delete;

  std::unique_ptr<apache::thrift::AsyncProcessor> getProcessor() override;

  fb303::cpp2::fb303_status getStatus() override;

  void mount(std::unique_ptr<MountArgument> mount) override;

  void unmount(std::unique_ptr<std::string> mountPoint) override;

  void listMounts(std::vector<MountInfo>& results) override;

  void checkOutRevision(
      std::vector<CheckoutConflict>& results,
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> hash,
      CheckoutMode checkoutMode,
      std::unique_ptr<CheckOutRevisionParams> params) override;

  void resetParentCommits(
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<WorkingDirectoryParents> parents,
      std::unique_ptr<ResetParentCommitsParams> params) override;

  void getBindMounts(
      std::vector<std::string>& out,
      std::unique_ptr<std::string> mountPointPtr) override;
  void addBindMount(
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> repoPath,
      std::unique_ptr<std::string> targetPath) override;
  void removeBindMount(
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> repoPath) override;

  void getSHA1(
      std::vector<SHA1Result>& out,
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::vector<std::string>> paths) override;

  void getCurrentJournalPosition(
      JournalPosition& out,
      std::unique_ptr<std::string> mountPoint) override;

  void getFilesChangedSince(
      FileDelta& out,
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<JournalPosition> fromPosition) override;

  void setJournalMemoryLimit(
      std::unique_ptr<PathString> mountPoint,
      int64_t limit) override;

  int64_t getJournalMemoryLimit(
      std::unique_ptr<PathString> mountPoint) override;

  void flushJournal(std::unique_ptr<PathString> mountPoint) override;

  void debugGetRawJournal(
      DebugGetRawJournalResponse& out,
      std::unique_ptr<DebugGetRawJournalParams> params) override;

  folly::SemiFuture<std::unique_ptr<std::vector<EntryInformationOrError>>>
  semifuture_getEntryInformation(
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::vector<std::string>> paths) override;

  folly::SemiFuture<std::unique_ptr<std::vector<FileInformationOrError>>>
  semifuture_getFileInformation(
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::vector<std::string>> paths) override;

  folly::Future<std::unique_ptr<Glob>> future_globFiles(
      std::unique_ptr<GlobParams> params) override;

  folly::Future<std::unique_ptr<Glob>> future_predictiveGlobFiles(
      std::unique_ptr<GlobParams> params) override;

  folly::Future<folly::Unit> future_chown(
      std::unique_ptr<std::string> mountPoint,
      int32_t uid,
      int32_t gid) override;

  apache::thrift::ServerStream<JournalPosition> subscribeStreamTemporary(
      std::unique_ptr<std::string> mountPoint) override;

#ifndef _WIN32
  apache::thrift::ServerStream<FsEvent> traceFsEvents(
      std::unique_ptr<std::string> mountPoint,
      int64_t eventCategoryMask) override;
#endif

  apache::thrift::ServerStream<HgEvent> traceHgEvents(
      std::unique_ptr<std::string> mountPoint) override;

  void async_tm_getScmStatusV2(
      std::unique_ptr<apache::thrift::HandlerCallback<
          std::unique_ptr<GetScmStatusResult>>> callback,
      std::unique_ptr<GetScmStatusParams> params) override;

  void async_tm_getScmStatus(
      std::unique_ptr<
          apache::thrift::HandlerCallback<std::unique_ptr<ScmStatus>>> callback,
      std::unique_ptr<std::string> mountPoint,
      bool listIgnored,
      std::unique_ptr<std::string> commitHash) override;

  folly::Future<std::unique_ptr<ScmStatus>> future_getScmStatusBetweenRevisions(
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> oldHash,
      std::unique_ptr<std::string> newHash) override;

  void debugGetScmTree(
      std::vector<ScmTreeEntry>& entries,
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> id,
      bool localStoreOnly) override;

  void debugGetScmBlob(
      std::string& data,
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> id,
      bool localStoreOnly) override;

  void debugGetScmBlobMetadata(
      ScmBlobMetadata& metadata,
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> id,
      bool localStoreOnly) override;

  void debugInodeStatus(
      std::vector<TreeInodeDebugInfo>& inodeInfo,
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> path,
      int64_t flags) override;

  void debugOutstandingFuseCalls(
      std::vector<FuseCall>& outstandingCalls,
      std::unique_ptr<std::string> mountPoint) override;

  void debugOutstandingNfsCalls(
      std::vector<NfsCall>& outstandingCalls,
      std::unique_ptr<std::string> mountPoint) override;

  void debugStartRecordingActivity(
      ActivityRecorderResult& result,
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> outputPath) override;

  void debugStopRecordingActivity(
      ActivityRecorderResult& result,
      std::unique_ptr<std::string> mountPoint,
      int64_t unique) override;

  void debugListActivityRecordings(
      ListActivityRecordingsResult& result,
      std::unique_ptr<std::string> mountPoint) override;

  void debugGetInodePath(
      InodePathDebugInfo& inodePath,
      std::unique_ptr<std::string> mountPoint,
      int64_t inodeNumber) override;

  void clearFetchCounts() override;

  void clearFetchCountsByMount(std::unique_ptr<std::string> mountPath) override;

  void getAccessCounts(GetAccessCountsResult& result, int64_t duration)
      override;

  void clearAndCompactLocalStore() override;

  void debugClearLocalStoreCaches() override;

  void debugCompactLocalStorage() override;

  int64_t unloadInodeForPath(
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> path,
      std::unique_ptr<TimeSpec> age) override;

  void flushStatsNow() override;

  folly::Future<folly::Unit> future_invalidateKernelInodeCache(
      std::unique_ptr<std::string> mountPoint,
      std::unique_ptr<std::string> path) override;

  void getStatInfo(
      InternalStats& result,
      std::unique_ptr<GetStatInfoParams> params) override;

  void enableTracing() override;
  void disableTracing() override;
  void getTracePoints(std::vector<TracePoint>& result) override;

  void injectFault(std::unique_ptr<FaultDefinition> fault) override;
  bool removeFault(std::unique_ptr<RemoveFaultArg> fault) override;
  int64_t unblockFault(std::unique_ptr<UnblockFaultArg> info) override;

  folly::Future<std::unique_ptr<SetPathObjectIdResult>> future_setPathObjectId(
      std::unique_ptr<SetPathObjectIdParams> params) override;

  void reloadConfig() override;

  void getDaemonInfo(DaemonInfo& result) override;

  /**
   * Checks the PrivHelper connection.
   * For Windows, result.connected will always be set to true.
   */
  void checkPrivHelper(PrivHelperInfo& result) override;

  int64_t getPid() override;

  /**
   * A thrift client has requested that we shutdown.
   */
  void initiateShutdown(std::unique_ptr<std::string> reason) override;

  void getConfig(
      EdenConfigData& result,
      std::unique_ptr<GetConfigParams> params) override;

  /**
   * Enable all backing stores to record fetched files
   */
  void startRecordingBackingStoreFetch() override;

  /**
   * Make all backing stores stop recording
   * fetched files. Previous records for different kinds of backing
   * stores will be returned by backing store types.
   */
  void stopRecordingBackingStoreFetch(GetFetchedFilesResult& results) override;

  /**
   * Returns the pid that caused the Thrift request running on the calling
   * Thrift worker thread and registers it with the ProcessNameCache.
   *
   * This must be run from a Thrift worker thread, because the calling pid is
   * stored in a thread local variable.
   */
  std::optional<pid_t> getAndRegisterClientPid();

 private:
  folly::Future<Hash> getSHA1ForPath(
      AbsolutePathPiece mountPoint,
      folly::StringPiece path,
      ObjectFetchContext& fetchContext);

  folly::Future<Hash> getSHA1ForPathDefensively(
      AbsolutePathPiece mountPoint,
      folly::StringPiece path,
      ObjectFetchContext& fetchContext) noexcept;

  folly::Future<std::unique_ptr<Glob>> _globFiles(
      folly::StringPiece mountPoint,
      std::vector<std::string> globs,
      bool includeDotfiles,
      bool prefetchFiles,
      bool suppressFileList,
      bool wantDtype,
      std::vector<std::string> revisions,
      bool prefetchMetadata,
      folly::StringPiece searchRootUser,
      bool background,
      folly::StringPiece caller,
      std::optional<pid_t> pid);

#ifdef __linux__
  // an endpoint for the edenfs/edenfs_service smartservice used for predictive
  // prefetch profiles
  std::unique_ptr<EdenFSSmartPlatformServiceEndpoint> spServiceEndpoint_;
#endif
  const std::vector<std::string> originalCommandLine_;
  EdenServer* const server_;
};
} // namespace eden
} // namespace facebook
