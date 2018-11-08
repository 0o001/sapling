/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include <folly/init/Init.h>
#include <folly/io/async/AsyncSignalHandler.h>
#include <folly/logging/Init.h>
#include <folly/logging/xlog.h>
#include <gflags/gflags.h>
#include <signal.h>
#include <thrift/lib/cpp2/server/ThriftServer.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <fstream>
#include <iterator>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include "eden/fs/eden-config.h"
#include "eden/fs/fuse/privhelper/UserInfo.h"
#include "eden/fs/service/StartupLogger.h"
#include "eden/fs/service/gen-cpp2/StreamingEdenService.h"
#include "eden/fs/utils/PathFuncs.h"

using namespace std::literals::chrono_literals;
using apache::thrift::ThriftServer;
using facebook::fb303::cpp2::fb_status;
using folly::EventBase;
using folly::StringPiece;
using std::make_shared;
using std::string;

DEFINE_bool(
    edenfs,
    false,
    "This argument must be supplied to confirm you intend to run "
    "edenfs instead of eden");
DEFINE_bool(allowRoot, false, "Allow running eden directly as root");
DEFINE_string(
    cleanShutdownFile,
    "",
    "If set, create the given file when shutting down cleanly");
DEFINE_string(
    commandArgumentsLogFile,
    "",
    "If set, write argv to the given file before starting up");
DEFINE_bool(
    exitWithoutCleanupOnStop,
    false,
    "Respond to stop requests by exiting abruptly");
#if EDEN_HAVE_SYSTEMD
DEFINE_bool(
    experimentalSystemd,
    false,
    "EXPERIMENTAL: Run edenfs as if systemd controls its lifecycle");
#endif
DEFINE_bool(
    failDuringStartup,
    false,
    "Instead of reporting success after starting up, report failure and exit");
DEFINE_bool(ignoreStop, false, "Ignore attempts to stop edenfs");
DEFINE_double(
    sleepBeforeGetPid,
    0.0,
    "Sleep for this many seconds before responding to getPid");
DEFINE_double(
    sleepBeforeStop,
    0.0,
    "Sleep for this many seconds before stopping");
DEFINE_string(edenDir, "", "The path to the .eden directory");
DEFINE_string(
    etcEdenDir,
    "/etc/eden",
    "The directory holding all system configuration files");
DEFINE_string(configPath, "", "The path of the ~/.edenrc config file");

using namespace facebook::eden;

FOLLY_INIT_LOGGING_CONFIG(".=INFO,eden=DBG2");

namespace {

class FakeEdenServiceHandler;

template <class Rep, class Ratio>
std::string prettyPrint(
    std::chrono::duration<Rep, Ratio> duration,
    bool addSpace = true);

void writeCommandArgumentsToLogIfNecessary(const std::vector<std::string>&);

enum class StopBehavior {
  DoNothing,
  ExitWithoutCleanup,
  TerminateEventLoop,
};

class FakeEdenServer {
 public:
  FakeEdenServer() {}

  void run(folly::SocketAddress thriftAddress, StartupLogger& startupLogger);
  void stop(StringPiece reason) {
    XLOG(INFO) << "received stop request: " << reason;

    if (stopSleepDuration_ > 0ms) {
      XLOG(INFO) << "pausing stop attempt for "
                 << prettyPrint(stopSleepDuration_);
      std::this_thread::sleep_for(stopSleepDuration_);
    }

    switch (stopBehavior_) {
      case StopBehavior::DoNothing:
        XLOG(INFO) << "ignoring stop attempt";
        break;
      case StopBehavior::ExitWithoutCleanup:
        XLOG(INFO) << "exiting without cleanup";
        _exit(1);
      case StopBehavior::TerminateEventLoop:
        XLOG(INFO) << "stopping";
        eventBase_->terminateLoopSoon();
        break;
    }
  }

  uint64_t getPid() const {
    if (getPidSleepDuration_ > 0ms) {
      XLOG(INFO) << "pausing getPid call for "
                 << prettyPrint(getPidSleepDuration_);
      std::this_thread::sleep_for(getPidSleepDuration_);
    }

    return getpid();
  }

  void setStopBehavior(StopBehavior stopBehavior) {
    stopBehavior_ = stopBehavior;
  }

  void setGetPidSleepDuration(std::chrono::milliseconds getPidSleepDuration) {
    getPidSleepDuration_ = getPidSleepDuration;
  }

  void setStopSleepDuration(std::chrono::milliseconds stopSleepDuration) {
    stopSleepDuration_ = stopSleepDuration;
  }

  void setCleanShutdownFile(AbsolutePath path) {
    cleanShutdownFile_ = path;
  }

 private:
  void reportCleanShutdown() {
    if (cleanShutdownFile_) {
      folly::File{cleanShutdownFile_->c_str(), O_WRONLY | O_CREAT};
    }
  }

  EventBase* eventBase_{nullptr};
  ThriftServer server_;
  std::shared_ptr<FakeEdenServiceHandler> handler_;
  StopBehavior stopBehavior_{StopBehavior::TerminateEventLoop};
  std::chrono::milliseconds getPidSleepDuration_{0ms};
  std::chrono::milliseconds stopSleepDuration_{0ms};
  std::optional<AbsolutePath> cleanShutdownFile_{};
};

class FakeEdenServiceHandler : virtual public StreamingEdenServiceSvIf {
 public:
  explicit FakeEdenServiceHandler(FakeEdenServer* server) : server_{server} {}

  fb_status getStatus() override {
    return status_;
  }

  void setOption(std::unique_ptr<string> name, std::unique_ptr<string> value)
      override {
    auto badOption = [&]() {
      auto errMsg = folly::to<string>(
          "invalid value for ", *name, " setting: \"", *value, "\"");
      XLOG(ERR) << errMsg;
      throw std::invalid_argument(errMsg);
    };

    if (*name == "honor_stop") {
      auto boolValue = folly::tryTo<bool>(*value);
      if (boolValue.hasError()) {
        badOption();
      }
      server_->setStopBehavior(
          boolValue.value() ? StopBehavior::TerminateEventLoop
                            : StopBehavior::DoNothing);
    } else if (*name == "status") {
      if (*value == "starting") {
        status_ = fb_status::STARTING;
      } else if (*value == "alive") {
        status_ = fb_status::ALIVE;
      } else if (*value == "stopping") {
        status_ = fb_status::STOPPING;
      } else {
        badOption();
      }
    }
  }

  int64_t getPid() override {
    return server_->getPid();
  }

  void listMounts(std::vector<MountInfo>& /* results */) override {
    return;
  }

  void shutdown() override {
    server_->stop("received shutdown() thrift request");
  }

  void initiateShutdown(std::unique_ptr<string> reason) override {
    server_->stop(folly::to<string>(
        "received initiateShutdown() thrift requested: ", reason->c_str()));
  }

 private:
  FakeEdenServer* server_{nullptr};
  fb_status status_{fb_status::ALIVE};
};

class SignalHandler : public folly::AsyncSignalHandler {
 public:
  SignalHandler(EventBase* eventBase, FakeEdenServer* server)
      : AsyncSignalHandler(eventBase), server_{server} {
    registerSignalHandler(SIGINT);
    registerSignalHandler(SIGTERM);
  }

  void signalReceived(int sig) noexcept override {
    switch (sig) {
      case SIGINT:
        server_->stop("received SIGINT");
        break;
      case SIGTERM:
        server_->stop("received SIGTERM");
        break;
      default:
        XLOG(INFO) << "received signal " << sig;
        break;
    }
  }

 private:
  FakeEdenServer* server_{nullptr};
};

void FakeEdenServer::run(
    folly::SocketAddress thriftAddress,
    StartupLogger& startupLogger) {
  eventBase_ = folly::EventBaseManager::get()->getEventBase();

  // Create the ThriftServer object
  auto handler = make_shared<FakeEdenServiceHandler>(this);
  server_.setInterface(handler);
  server_.setAddress(thriftAddress);

  // Set up a signal handler to ignore SIGINT and SIGTERM
  // This lets our integration tests exercise the case where edenfs does not
  // shut down on its own.
  SignalHandler signalHandler(eventBase_, this);

  // Run the thrift server
  server_.setup();
  if (FLAGS_failDuringStartup) {
    startupLogger.exitUnsuccessfully(
        1,
        "Started successfully, but reporting failure because --failDuringStartup was specified");
  }
  startupLogger.success();
  eventBase_->loopForever();

  reportCleanShutdown();
}

bool acquireLock(AbsolutePathPiece edenDir) {
  const auto lockPath = edenDir + "lock"_pc;
  auto lockFile = folly::File(lockPath.value(), O_WRONLY | O_CREAT);
  if (!lockFile.try_lock()) {
    return false;
  }

  // Write the PID (with a newline) to the lockfile.
  folly::ftruncateNoInt(lockFile.fd(), /* len */ 0);
  const auto pidContents = folly::to<std::string>(getpid(), "\n");
  folly::writeNoInt(lockFile.fd(), pidContents.data(), pidContents.size());

  // Intentionally leak the lock FD so we hold onto it until we exit.
  lockFile.release();
  return true;
}

} // namespace

int main(int argc, char** argv) {
  // Drop privileges
  auto identity = UserInfo::lookup();
  identity.dropPrivileges();

  auto originalCommandArguments =
      std::vector<std::string>{&argv[0], &argv[argc]};

  auto init = folly::Init(&argc, &argv);

  writeCommandArgumentsToLogIfNecessary(originalCommandArguments);

#if EDEN_HAVE_SYSTEMD
  if (FLAGS_experimentalSystemd) {
    XLOG(INFO) << "Running in experimental systemd mode";
  }
#endif

  if (argc != 1) {
    fprintf(stderr, "error: unexpected trailing command line arguments\n");
    return 1;
  }
  if (!FLAGS_edenfs) {
    XLOG(ERR) << "the --edenfs flag is required";
    return 1;
  }
  if (FLAGS_edenDir.empty()) {
    XLOG(ERR) << "the --edenDir flag is required";
    return 1;
  }
  auto edenDir = facebook::eden::canonicalPath(FLAGS_edenDir);

  auto logPath = edenDir + "edenfs.log"_pc;
  auto startupLogger = daemonizeIfRequested(logPath.value());

  // Acquire the lock file
  if (!acquireLock(edenDir)) {
    startupLogger->exitUnsuccessfully(1, "Failed to acquire lock file");
  }

  startupLogger->log("Starting fake edenfs daemon");

  // Get the path to the thrift socket.
  auto thriftSocketPath = edenDir + "socket"_pc;
  folly::SocketAddress thriftAddress;
  thriftAddress.setFromPath(thriftSocketPath.stringPiece());

  // Make sure no socket already exists at this path
  int rc = unlink(thriftSocketPath.value().c_str());
  if (rc != 0 && errno != ENOENT) {
    int errnum = errno;
    startupLogger->exitUnsuccessfully(
        1,
        "failed to remove eden socket at ",
        thriftSocketPath,
        ": ",
        folly::errnoStr(errnum));
  }

  FakeEdenServer server;
  if (!FLAGS_cleanShutdownFile.empty()) {
    server.setCleanShutdownFile(AbsolutePath{FLAGS_cleanShutdownFile});
  }
  if (FLAGS_ignoreStop) {
    server.setStopBehavior(StopBehavior::DoNothing);
  }
  if (FLAGS_exitWithoutCleanupOnStop) {
    server.setStopBehavior(StopBehavior::ExitWithoutCleanup);
  }
  server.setGetPidSleepDuration(
      std::chrono::duration_cast<std::chrono::milliseconds>(
          std::chrono::duration<double>{FLAGS_sleepBeforeGetPid}));
  server.setStopSleepDuration(
      std::chrono::duration_cast<std::chrono::milliseconds>(
          std::chrono::duration<double>{FLAGS_sleepBeforeStop}));
  server.run(thriftAddress, *startupLogger);
  return 0;
}

namespace {

template <class Rep, class Ratio>
std::string prettyPrint(
    std::chrono::duration<Rep, Ratio> duration,
    bool addSpace) {
  auto durationInSeconds =
      std::chrono::duration_cast<std::chrono::duration<double>>(duration);
  return folly::prettyPrint(
      durationInSeconds.count(), folly::PRETTY_TIME, addSpace);
}

void writeCommandArgumentsToLogIfNecessary(
    const std::vector<std::string>& originalCommandArguments) {
  const auto& path = FLAGS_commandArgumentsLogFile;
  if (path.empty()) {
    return;
  }
  auto file = std::ofstream{};
  file.exceptions(std::ofstream::badbit | std::ofstream::failbit);
  file.open(path);
  std::copy(
      originalCommandArguments.begin(),
      originalCommandArguments.end(),
      std::ostream_iterator<std::string>{file, "\n"});
}

} // namespace
