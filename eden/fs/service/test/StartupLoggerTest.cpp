/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/service/StartupLogger.h"

#include <folly/Exception.h>
#include <folly/File.h>
#include <folly/FileUtil.h>
#include <folly/Subprocess.h>
#include <folly/experimental/TestUtil.h>
#include <folly/init/Init.h>
#include <folly/logging/xlog.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>
#include <signal.h>
#include <sysexits.h>
#include <cerrno>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <optional>
#include <string>
#include <thread>

using namespace facebook::eden;
using namespace std::chrono_literals;
using folly::checkUnixError;
using folly::File;
using folly::StringPiece;
using folly::test::TemporaryFile;
using std::string;
using testing::ContainsRegex;
using testing::HasSubstr;
using testing::Not;

namespace facebook {
namespace eden {

namespace {
struct FunctionResult {
  std::string standardOutput;
  std::string standardError;
  folly::ProcessReturnCode returnCode;
};

FunctionResult runFunctionInSeparateProcess(folly::StringPiece functionName);
folly::ProcessReturnCode waitWithTimeout(
    folly::Subprocess&,
    std::chrono::milliseconds timeout);
bool isReadablePipeBroken(int fd);
bool isWritablePipeBroken(int fd);
} // namespace

class DaemonStartupLoggerTest : public ::testing::Test {
 protected:
  /*
   * Use DaemonStartupLogger::daemonizeImpl() to run the specified function in
   * the child process.
   *
   * Returns the ParentResult object in the parent process.
   */
  template <typename Fn>
  DaemonStartupLogger::ParentResult runDaemonize(Fn&& fn) {
    DaemonStartupLogger logger;
    auto parentInfo = logger.daemonizeImpl(logPath());
    if (parentInfo) {
      // parent
      auto pid = parentInfo->first;
      auto& readPipe = parentInfo->second;
      return logger.waitForChildStatus(std::move(readPipe), pid, logPath());
    }

    // child
    try {
      fn(std::move(logger));
    } catch (const std::exception& ex) {
      XLOG(ERR) << "unexpected error: " << folly::exceptionStr(ex);
      exit(1);
    }
    exit(0);
  }

  // Wrappers simply to allow our tests to access private DaemonStartupLogger
  // methods
  File createPipe(DaemonStartupLogger& logger) {
    return logger.createPipe();
  }
  void closePipe(DaemonStartupLogger& logger) {
    logger.pipe_.close();
  }
  DaemonStartupLogger::ParentResult waitForChildStatus(
      DaemonStartupLogger& logger,
      File readPipe,
      pid_t childPid,
      StringPiece logPath) {
    return logger.waitForChildStatus(std::move(readPipe), childPid, logPath);
  }

  string logPath() {
    return logFile_.path().string();
  }
  string readLogContents() {
    string logContents;
    if (!folly::readFile(logPath().c_str(), logContents)) {
      throw std::runtime_error(folly::to<string>(
          "error reading from log file ",
          logPath(),
          ": ",
          folly::errnoStr(errno)));
    }
    return logContents;
  }

  TemporaryFile logFile_{"eden_test_log"};
};

TEST_F(DaemonStartupLoggerTest, crashWithNoResult) {
  // Fork a child that just kills itself
  auto result = runDaemonize([](DaemonStartupLogger&&) {
    fprintf(stderr, "this message should go to the log\n");
    fflush(stderr);
    kill(getpid(), SIGKILL);
    // Wait until we get killed.
    while (true) {
      /* sleep override */ std::this_thread::sleep_for(30s);
    }
  });
  EXPECT_EQ(EX_SOFTWARE, result.exitCode);
  EXPECT_EQ(
      folly::to<string>(
          "error: edenfs crashed with signal ",
          SIGKILL,
          " before it finished initializing\n"
          "Check the edenfs log file at ",
          logPath(),
          " for more details"),
      result.errorMessage);

  // Verify that the log message from the child went to the log file
  EXPECT_EQ("this message should go to the log\n", readLogContents());
}

TEST_F(DaemonStartupLoggerTest, successWritesStartedMessageToStandardError) {
  auto result = runFunctionInSeparateProcess(
      "successWritesStartedMessageToStandardErrorDaemonChild");
  EXPECT_THAT(
      result.standardError, ContainsRegex("Started edenfs \\(pid [0-9]+\\)"));
  EXPECT_THAT(result.standardError, HasSubstr("Logs available at "));
}

void successWritesStartedMessageToStandardErrorDaemonChild() {
  auto logFile = TemporaryFile{"eden_test_log"};
  auto logger = DaemonStartupLogger{};
  logger.daemonize(logFile.path().string());
  logger.success();
  exit(0);
}

TEST_F(DaemonStartupLoggerTest, exitWithNoResult) {
  // Fork a child that exits unsuccessfully
  auto result = runDaemonize([](DaemonStartupLogger&&) { _exit(19); });

  EXPECT_EQ(19, result.exitCode);
  EXPECT_EQ(
      folly::to<string>(
          "error: edenfs exited with status 19 before it finished initializing\n"
          "Check the edenfs log file at ",
          logPath(),
          " for more details"),
      result.errorMessage);
}

TEST_F(DaemonStartupLoggerTest, exitSuccessfullyWithNoResult) {
  // Fork a child that exits successfully
  auto result = runDaemonize([](DaemonStartupLogger&&) { _exit(0); });

  // The parent process should be EX_SOFTWARE in this case
  EXPECT_EQ(EX_SOFTWARE, result.exitCode);
  EXPECT_EQ(
      folly::to<string>(
          "error: edenfs exited with status 0 before it finished initializing\n"
          "Check the edenfs log file at ",
          logPath(),
          " for more details"),
      result.errorMessage);
}

TEST_F(DaemonStartupLoggerTest, destroyLoggerWhileDaemonIsStillRunning) {
  // Fork a child process that will destroy the DaemonStartupLogger and then
  // wait until we tell it to exit.
  std::array<int, 2> pipeFDs;
  auto rc = pipe2(pipeFDs.data(), O_CLOEXEC);
  checkUnixError(rc, "failed to create pipes");
  folly::File readPipe(pipeFDs[0], /*ownsFd=*/true);
  folly::File writePipe(pipeFDs[1], /*ownsFd=*/true);

  auto result =
      runDaemonize([&readPipe, &writePipe](DaemonStartupLogger&& logger) {
        writePipe.close();

        // Destroy the DaemonStartupLogger object to force it to close its pipes
        // without sending a result.
        std::optional<DaemonStartupLogger> optLogger(std::move(logger));
        optLogger.reset();

        // Wait for the parent process to signal us to exit.
        // We do so by calling readFull(). It will return when the pipe has
        // closed.
        uint8_t byte;
        auto readResult = folly::readFull(readPipe.fd(), &byte, sizeof(byte));
        checkUnixError(
            readResult, "error reading close signal from parent process");
      });

  EXPECT_EQ(EX_SOFTWARE, result.exitCode);
  EXPECT_EQ(
      folly::to<std::string>(
          "error: edenfs is still running but "
          "did not report its initialization status\n"
          "Check the edenfs log file at ",
          logPath(),
          " for more details"),
      result.errorMessage);

  // Close the write end of the pipe so the child process will quit
  writePipe.close();
}

TEST_F(DaemonStartupLoggerTest, closePipeWithWaitError) {
  // Call waitForChildStatus() with our own pid.
  // wait() will return an error trying to wait on ourself.
  DaemonStartupLogger logger;
  auto readPipe = createPipe(logger);
  closePipe(logger);
  auto result = waitForChildStatus(
      logger, std::move(readPipe), getpid(), "/var/log/edenfs.log");

  EXPECT_EQ(EX_SOFTWARE, result.exitCode);
  EXPECT_EQ(
      "error: edenfs did not report its initialization status\n"
      "error: error checking status of edenfs daemon: No child processes\n"
      "Check the edenfs log file at /var/log/edenfs.log for more details",
      result.errorMessage);
}

TEST_F(DaemonStartupLoggerTest, success) {
  auto result =
      runDaemonize([](DaemonStartupLogger&& logger) { logger.success(); });
  EXPECT_EQ(0, result.exitCode);
  EXPECT_EQ("", result.errorMessage);
}

TEST_F(DaemonStartupLoggerTest, failure) {
  auto result = runDaemonize([](DaemonStartupLogger&& logger) {
    logger.exitUnsuccessfully(3, "example failure for tests");
  });
  EXPECT_EQ(3, result.exitCode);
  EXPECT_EQ("", result.errorMessage);
  EXPECT_THAT(readLogContents(), HasSubstr("example failure for tests"));
}

TEST_F(DaemonStartupLoggerTest, daemonClosesStandardFileDescriptors) {
  auto process = folly::Subprocess{
      {{
          folly::fs::executable_path().string(),
          "daemonClosesStandardFileDescriptorsChild",
      }},
      folly::Subprocess::Options{}.pipeStdin().pipeStdout().pipeStderr(),
  };
  SCOPE_FAIL {
    process.takeOwnershipOfPipes();
    process.wait();
  };
  process.setAllNonBlocking();

  // FIXME(strager): wait() could technically deadlock if the child is blocked
  // on writing to stdout or stderr.
  auto returnCode = waitWithTimeout(process, 10s);
  EXPECT_EQ("exited with status 0", returnCode.str());

  auto expectReadablePipeIsBroken = [](int fd, folly::StringPiece name) {
    EXPECT_TRUE(isReadablePipeBroken(fd))
        << "Daemon should have closed its " << name
        << " file descriptor (parent fd " << fd << "), but it did not.";
  };
  auto expectWritablePipeIsBroken = [](int fd, folly::StringPiece name) {
    EXPECT_TRUE(isWritablePipeBroken(fd))
        << "Daemon should have closed its " << name
        << " file descriptor (parent fd " << fd << "), but it did not.";
  };

  expectWritablePipeIsBroken(process.stdinFd(), "stdin");
  expectReadablePipeIsBroken(process.stdoutFd(), "stdout");
  expectReadablePipeIsBroken(process.stderrFd(), "stderr");

  // NOTE(strager): The daemon process should eventually exit automatically, so
  // we don't need to explicitly kill it.
}

void daemonClosesStandardFileDescriptorsChild() {
  auto logFile = TemporaryFile{"eden_test_log"};
  auto logger = DaemonStartupLogger{};
  logger.daemonize(logFile.path().string());
  logger.success();
  std::this_thread::sleep_for(30s);
  exit(1);
}

TEST(ForegroundStartupLoggerTest, loggedMessagesAreWrittenToStandardError) {
  auto result = runFunctionInSeparateProcess(
      "loggedMessagesAreWrittenToStandardErrorChild");
  EXPECT_THAT(result.standardOutput, Not(HasSubstr("warn message")));
  EXPECT_THAT(result.standardError, HasSubstr("warn message"));
}

void loggedMessagesAreWrittenToStandardErrorChild() {
  auto logger = ForegroundStartupLogger{};
  logger.warn("warn message");
}

TEST(ForegroundStartupLoggerTest, exitUnsuccessfullyMakesProcessExitWithCode) {
  auto result = runFunctionInSeparateProcess(
      "exitUnsuccessfullyMakesProcessExitWithCodeChild");
  EXPECT_EQ("exited with status 42", result.returnCode.str());
}

void exitUnsuccessfullyMakesProcessExitWithCodeChild() {
  auto logger = ForegroundStartupLogger{};
  logger.exitUnsuccessfully(42, "intentionally exiting");
}

TEST(ForegroundStartupLoggerTest, xlogsAfterSuccessAreWrittenToStandardError) {
  auto result = runFunctionInSeparateProcess(
      "xlogsAfterSuccessAreWrittenToStandardErrorChild");
  EXPECT_THAT(result.standardError, HasSubstr("test error message with xlog"));
}

void xlogsAfterSuccessAreWrittenToStandardErrorChild() {
  auto logger = ForegroundStartupLogger{};
  logger.success();
  XLOG(ERR) << "test error message with xlog";
}

TEST(ForegroundStartupLoggerTest, successWritesStartedMessageToStandardError) {
  auto result = runFunctionInSeparateProcess(
      "successWritesStartedMessageToStandardErrorForegroundChild");
  EXPECT_THAT(
      result.standardError,
      ContainsRegex("Started edenfs \\(pid [0-9]+\\)\n$"));
}

void successWritesStartedMessageToStandardErrorForegroundChild() {
  auto logger = ForegroundStartupLogger{};
  logger.success();
}

namespace {
FunctionResult runFunctionInSeparateProcess(folly::StringPiece functionName) {
  auto process = folly::Subprocess{
      std::vector<std::string>{{
          folly::fs::executable_path().string(),
          std::string{functionName},
      }},
      folly::Subprocess::Options{}.pipeStdout().pipeStderr(),
  };
  SCOPE_FAIL {
    process.takeOwnershipOfPipes();
    process.wait();
  };
  auto [out, err] = process.communicate();
  auto returnCode = process.wait();
  return FunctionResult{out, err, returnCode};
}

[[noreturn]] void runFunctionInCurrentProcess(folly::StringPiece functionName) {
  auto checkFunction = [&](folly::StringPiece name, void (*function)()) {
    if (functionName == name) {
      function();
      std::exit(0);
    }
  };
#define CHECK_FUNCTION(name) checkFunction(#name, name)
  CHECK_FUNCTION(daemonClosesStandardFileDescriptorsChild);
  CHECK_FUNCTION(exitUnsuccessfullyMakesProcessExitWithCodeChild);
  CHECK_FUNCTION(loggedMessagesAreWrittenToStandardErrorChild);
  CHECK_FUNCTION(successWritesStartedMessageToStandardErrorDaemonChild);
  CHECK_FUNCTION(successWritesStartedMessageToStandardErrorForegroundChild);
  CHECK_FUNCTION(xlogsAfterSuccessAreWrittenToStandardErrorChild);
#undef CHECK_FUNCTION
  std::fprintf(
      stderr,
      "error: unknown function: %s\n",
      std::string{functionName}.c_str());
  std::exit(2);
}

folly::ProcessReturnCode waitWithTimeout(
    folly::Subprocess& process,
    std::chrono::milliseconds timeout) {
  auto deadline = std::chrono::steady_clock::now() + timeout;
  do {
    auto returnCode = process.poll();
    if (!returnCode.running()) {
      return returnCode;
    }
    std::this_thread::sleep_for(1ms);
  } while (std::chrono::steady_clock::now() < deadline);
  return {};
}

bool isReadablePipeBroken(int fd) {
drain_pipe:
  char buffer[PIPE_BUF];
  ssize_t readSize = folly::readNoInt(fd, buffer, sizeof(buffer));
  if (readSize == -1 && errno == EAGAIN) {
    return false;
  }
  checkUnixError(readSize);
  if (readSize == 0) {
    return true;
  }
  goto drain_pipe;
}

bool isWritablePipeBroken(int fd) {
  const char buffer[1] = {0};
  ssize_t writtenSize = folly::writeNoInt(fd, buffer, sizeof(buffer));
  if (writtenSize == -1 && errno == EAGAIN) {
    return false;
  }
  if (writtenSize == -1 && errno == EPIPE) {
    return true;
  }
  checkUnixError(writtenSize);
  return false;
}

} // namespace

} // namespace eden
} // namespace facebook

int main(int argc, char** argv) {
  ::testing::InitGoogleTest(&argc, argv);
  auto removeFlags = true;
  auto initGuard = folly::Init(&argc, &argv, removeFlags);
  if (argc >= 2) {
    runFunctionInCurrentProcess(argv[1]);
  }
  return RUN_ALL_TESTS();
}
