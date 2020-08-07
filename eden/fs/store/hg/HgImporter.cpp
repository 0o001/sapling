/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/store/hg/HgImporter.h"

#include <boost/filesystem/operations.hpp>
#include <boost/filesystem/path.hpp>
#include <folly/Conv.h>
#include <folly/FileUtil.h>
#include <folly/Utility.h>
#include <folly/container/Array.h>
#include <folly/dynamic.h>
#include <folly/experimental/EnvUtil.h>
#include <folly/futures/Future.h>
#include <folly/io/Cursor.h>
#include <folly/io/IOBuf.h>
#include <folly/json.h>
#include <folly/lang/Bits.h>
#include <folly/logging/xlog.h>
#include <gflags/gflags.h>
#include <glog/logging.h>
#ifndef _WIN32
#include <unistd.h>
#else
#include "eden/fs/win/utils/Pipe.h" // @manual
#include "eden/fs/win/utils/Subprocess.h" // @manual
#include "eden/fs/win/utils/WinError.h" // @manual
#endif

#include <mutex>

#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/model/TreeEntry.h"
#include "eden/fs/store/hg/HgImportPyError.h"
#include "eden/fs/store/hg/HgProxyHash.h"
#include "eden/fs/telemetry/EdenStats.h"
#include "eden/fs/utils/PathFuncs.h"
#include "eden/fs/utils/TimeUtil.h"

using folly::Endian;
using folly::IOBuf;
using folly::StringPiece;
#ifndef _WIN32
using folly::Subprocess;
#else
using facebook::eden::Pipe;
using facebook::eden::Subprocess;
#endif
using folly::io::Appender;
using folly::io::Cursor;
using std::make_unique;
using std::string;
using std::unique_ptr;

#ifdef _WIN32
// We will use the known path to HG executable instead of searching in the
// path. This would make sure we are picking the right mercurial. In future
// we should find a chef config to lookup the path.

DEFINE_string(
    hgPath,
    "C:\\tools\\hg\\hg.real.exe",
    "The path to the mercurial executable");
#else
DEFINE_string(hgPath, "hg.real", "The path to the mercurial executable");
#endif

DEFINE_string(
    hgPythonPath,
    "",
    "Value to use for the PYTHONPATH when running mercurial import script. If "
    "this value is non-empty, the existing PYTHONPATH from the environment is "
    "replaced with this value.");

namespace {
using namespace facebook::eden;

/**
 * File descriptor number to use for receiving output from the import helper
 * process.
 *
 * This value is rather arbitrary.  It shouldn't be 0, 1, or 2 (stdin, stdout,
 * or stderr, respectively), but other than that anything is probably fine,
 * since the child shouldn't have any FDs open besides these 3 standard FDs
 * when it starts.
 *
 * The only reason we don't simply use the child's stdout is to avoid
 * communication problems if any of the mercurial helper code somehow ends up
 * printing data to stdout.  We don't want arbitrary log message data from
 * mercurial interfering with our normal communication protocol.
 */
constexpr int HELPER_PIPE_FD = 5;

#ifndef _WIN32
std::string findInPath(folly::StringPiece executable) {
  auto path = getenv("PATH");
  if (!path) {
    throw std::runtime_error(folly::to<std::string>(
        "unable to resolve ", executable, " in PATH because PATH is not set"));
  }
  std::vector<folly::StringPiece> dirs;
  folly::split(":", path, dirs);

  for (auto& dir : dirs) {
    auto candidate = folly::to<std::string>(dir, "/", executable);
    if (access(candidate.c_str(), X_OK) == 0) {
      return candidate;
    }
  }

  throw std::runtime_error(folly::to<std::string>(
      "unable to resolve ", executable, " in PATH ", path));
}
#endif

} // unnamed namespace

namespace facebook {
namespace eden {

class HgImporterEofError : public HgImporterError {
 public:
  using HgImporterError::HgImporterError;
};

HgImporter::HgImporter(
    AbsolutePathPiece repoPath,
    std::shared_ptr<EdenStats> stats,
    std::optional<AbsolutePath> importHelperScript)
    : repoPath_{repoPath}, stats_{std::move(stats)} {
  std::vector<string> cmd;

  // importHelperScript takes precedence if it was specified; this is used
  // primarily in our integration tests.
  if (importHelperScript.has_value()) {
    cmd.push_back(importHelperScript.value().value());
    cmd.push_back(repoPath.value().str());
  } else {
    cmd.push_back(FLAGS_hgPath);
    cmd.push_back("debugedenimporthelper");
  }

#ifndef _WIN32
  cmd.push_back("--out-fd");
  cmd.push_back(folly::to<string>(HELPER_PIPE_FD));

  // In the future, it might be better to use some other arbitrary fd for
  // output from the helper process, rather than stdout (just in case anything
  // in the python code ends up printing to stdout).
  Subprocess::Options opts;
  // Send commands to the child on its stdin.
  // Receive output on HELPER_PIPE_FD.
  opts.stdinFd(Subprocess::PIPE).fd(HELPER_PIPE_FD, Subprocess::PIPE_OUT);

  // Ensure that we run the helper process with cwd set to the repo.
  // This is important for `hg debugedenimporthelper` to pick up the
  // correct configuration in the currently available versions of
  // that subcommand.  In particular, without this, the tests may
  // fail when run in our CI environment.
  opts.chdir(repoPath.value().str());

  // If argv[0] isn't an absolute path then we need to search $PATH.
  // Ideally we'd just tell Subprocess to usePath, but it doesn't
  // allow us to do so when we are also overriding the environment.
  if (!boost::filesystem::path(cmd[0]).is_absolute()) {
    cmd[0] = findInPath(cmd[0]);
  }

  auto env = folly::experimental::EnvironmentState::fromCurrentEnvironment();
  if (!FLAGS_hgPythonPath.empty()) {
    env->erase("PYTHONPATH");
    env->emplace("PYTHONPATH", FLAGS_hgPythonPath);
  }

  // Eden does not control the backing repo's configuration, if it has
  // fsmonitor enabled, it might try to run Watchman, which might
  // cause Watchman to spawn a daemon instance, which might attempt to
  // access the FUSE mount, which might be in the process of starting
  // up. This causes a cross-process deadlock. Thus, in a heavy-handed
  // way, prevent Watchman from ever attempting to spawn an instance.
  (*env)["WATCHMAN_NO_SPAWN"] = "1";
  cmd.insert(
      cmd.end(),
      {"--config",
       "extensions.fsmonitor=!",
       "--config",
       "extensions.hgevents=!"});

  // HACK(T33686765): Work around LSAN reports for hg_importer_helper.
  (*env)["LSAN_OPTIONS"] = "detect_leaks=0";
  // If we're using `hg debugedenimporthelper`, don't allow the user
  // configuration to change behavior away from the system defaults.
  (*env)["HGPLAIN"] = "1";
  (*env)["CHGDISABLE"] = "1";

  auto envVector = env.toVector();
  helper_ = Subprocess{cmd, opts, nullptr, &envVector};
  SCOPE_FAIL {
    helper_.closeParentFd(STDIN_FILENO);
    helper_.wait();
  };
  helperIn_ = helper_.stdinFd();
  helperOut_ = helper_.parentFd(HELPER_PIPE_FD);
#else

  auto childInPipe = std::make_unique<Pipe>();
  auto childOutPipe = std::make_unique<Pipe>();

  cmd.push_back("--out-fd");
  cmd.push_back(folly::to<string>((intptr_t)childOutPipe->writeHandle()));
  cmd.push_back("--in-fd");
  cmd.push_back(folly::to<string>((intptr_t)childInPipe->readHandle()));

  helper_.createSubprocess(
      cmd,
      repoPath.value().str().c_str(),
      std::move(childInPipe),
      std::move(childOutPipe));
  helperIn_ = helper_.childInPipe_->writeHandle();
  helperOut_ = helper_.childOutPipe_->readHandle();

#endif
  options_ = waitForHelperStart();
  XLOG(DBG1) << "hg_import_helper started for repository " << repoPath_;
}

ImporterOptions HgImporter::waitForHelperStart() {
  // Wait for the import helper to send the CMD_STARTED message indicating
  // that it has started successfully.
  ChunkHeader header;
  try {
    header = readChunkHeader(0, "CMD_STARTED");
  } catch (const HgImporterEofError&) {
    // If we get EOF trying to read the initial response this generally
    // indicates that the import helper exited with an error early on during
    // startup, before it could send us a success or error message.
    //
    // It should have normally printed an error message to stderr in this case,
    // which is normally redirected to our edenfs.log file.
    throw HgImporterError(
        "error starting Mercurial import helper. Run `edenfsctl debug log` to "
        "view the error messages from the import helper.");
  }

  if (header.command != CMD_STARTED) {
    // This normally shouldn't happen.  If an error occurs, the
    // hg_import_helper script should send an error chunk causing
    // readChunkHeader() to throw an exception with the the error message
    // sent back by the script.
    throw std::runtime_error(
        "unexpected start message from hg_import_helper script");
  }

  if (header.dataLength < sizeof(uint32_t)) {
    throw std::runtime_error(
        "missing CMD_STARTED response body from hg_import_helper script");
  }

  IOBuf buf(IOBuf::CREATE, header.dataLength);

  readFromHelper(
      buf.writableTail(), header.dataLength, "CMD_STARTED response body");
  buf.append(header.dataLength);

  Cursor cursor(&buf);
  auto protocolVersion = cursor.readBE<uint32_t>();
  if (protocolVersion != PROTOCOL_VERSION) {
    throw std::runtime_error(folly::to<string>(
        "hg_import_helper protocol version mismatch: edenfs expected ",
        static_cast<uint32_t>(PROTOCOL_VERSION),
        ", hg_import_helper is speaking ",
        protocolVersion));
  }

  ImporterOptions options;

  auto flags = cursor.readBE<uint32_t>();
  auto numTreemanifestPaths = cursor.readBE<uint32_t>();
  if (!(flags & StartFlag::TREEMANIFEST_SUPPORTED)) {
    throw std::runtime_error(
        "hg_import_helper indicated that treemanifest is not supported. "
        "EdenFS requires treemanifest support.");
  }
  if (numTreemanifestPaths == 0) {
    throw std::runtime_error(
        "hg_import_helper indicated that treemanifest "
        "is supported, but provided no store paths");
  }
  for (uint32_t n = 0; n < numTreemanifestPaths; ++n) {
    auto pathLength = cursor.readBE<uint32_t>();
    options.treeManifestPackPaths.push_back(cursor.readFixedString(pathLength));
  }

  if (flags & StartFlag::MONONOKE_SUPPORTED) {
    auto nameLength = cursor.readBE<uint32_t>();
    options.repoName = cursor.readFixedString(nameLength);
  }

  return options;
}

HgImporter::~HgImporter() {
  stopHelperProcess();
}

#ifndef _WIN32
folly::ProcessReturnCode HgImporter::debugStopHelperProcess() {
  stopHelperProcess();
  return helper_.returnCode();
}
#endif

void HgImporter::stopHelperProcess() {
#ifndef _WIN32
  if (helper_.returnCode().running()) {
    helper_.closeParentFd(STDIN_FILENO);
    helper_.wait();
  }
#endif
}

unique_ptr<Blob> HgImporter::importFileContents(
    RelativePathPiece path,
    Hash blobHash) {
  XLOG(DBG5) << "requesting file contents of '" << path << "', "
             << blobHash.toString();

  // Ask the import helper process for the file contents
  auto requestID = sendFileRequest(path, blobHash);

  // Read the response.  The response body contains the file contents,
  // which is exactly what we want to return.
  //
  // Note: For now we expect to receive the entire contents in a single chunk.
  // In the future we might want to consider if it is more efficient to receive
  // the body data in fixed-size chunks, particularly for very large files.
  auto header = readChunkHeader(requestID, "CMD_CAT_FILE");
  if (header.dataLength < sizeof(uint64_t)) {
    auto msg = folly::to<string>(
        "CMD_CAT_FILE response for blob ",
        blobHash,
        " (",
        path,
        ", ",
        blobHash,
        ") from debugedenimporthelper is too "
        "short for body length field: length = ",
        header.dataLength);
    XLOG(ERR) << msg;
    throw std::runtime_error(std::move(msg));
  }
  auto buf = IOBuf(IOBuf::CREATE, header.dataLength);

  readFromHelper(
      buf.writableTail(), header.dataLength, "CMD_CAT_FILE response body");
  buf.append(header.dataLength);

  // The last 8 bytes of the response are the body length.
  // Ensure that this looks correct, and advance the buffer past this data to
  // the start of the actual response body.
  //
  // This data doesn't really need to be present in the response.  It is only
  // here so we can double-check that the response data appears valid.
  buf.trimEnd(sizeof(uint64_t));
  uint64_t bodyLength;
  memcpy(&bodyLength, buf.tail(), sizeof(uint64_t));
  bodyLength = Endian::big(bodyLength);
  if (bodyLength != header.dataLength - sizeof(uint64_t)) {
    auto msg = folly::to<string>(
        "inconsistent body length received when importing blob ",
        blobHash,
        " (",
        path,
        ", ",
        blobHash,
        "): bodyLength=",
        bodyLength,
        " responseLength=",
        header.dataLength);
    XLOG(ERR) << msg;
    throw std::runtime_error(std::move(msg));
  }

  XLOG(DBG4) << "imported blob " << blobHash << " (" << path << ", " << blobHash
             << "); length=" << bodyLength;

  return make_unique<Blob>(blobHash, std::move(buf));
}

void HgImporter::prefetchFiles(const std::vector<HgProxyHash>& files) {
  auto requestID = sendPrefetchFilesRequest(files);

  // Read the response; throws if there was any error.
  // No payload is returned.
  readChunkHeader(requestID, "CMD_PREFETCH_FILES");
}

void HgImporter::fetchTree(RelativePathPiece path, Hash pathManifestNode) {
  // Ask the hg_import_helper script to fetch data for this tree
  XLOG(DBG1) << "fetching data for tree \"" << path << "\" at manifest node "
             << pathManifestNode;
  auto requestID = sendFetchTreeRequest(path, pathManifestNode);

  ChunkHeader header;
  header = readChunkHeader(requestID, "CMD_FETCH_TREE");

  if (header.dataLength != 0) {
    throw std::runtime_error(folly::to<string>(
        "got unexpected length ",
        header.dataLength,
        " for FETCH_TREE response"));
  }
}

Hash HgImporter::resolveManifestNode(folly::StringPiece revName) {
  auto requestID = sendManifestNodeRequest(revName);

  auto header = readChunkHeader(requestID, "CMD_MANIFEST_NODE_FOR_COMMIT");
  if (header.dataLength != 20) {
    throw std::runtime_error(folly::to<string>(
        "expected a 20-byte hash for the manifest node '",
        revName,
        "' but got data of length ",
        header.dataLength));
  }

  Hash::Storage buffer;
  readFromHelper(
      buffer.data(),
      folly::to_narrow(buffer.size()),
      "CMD_MANIFEST_NODE_FOR_COMMIT response body");
  return Hash(buffer);
}

HgImporter::ChunkHeader HgImporter::readChunkHeader(
    TransactionID txnID,
    StringPiece cmdName) {
  ChunkHeader header;
  readFromHelper(&header, folly::to_narrow(sizeof(header)), "response header");

  header.requestID = Endian::big(header.requestID);
  header.command = Endian::big(header.command);
  header.flags = Endian::big(header.flags);
  header.dataLength = Endian::big(header.dataLength);

  // If the header indicates an error, read the error message
  // and throw an exception.
  if ((header.flags & FLAG_ERROR) != 0) {
    readErrorAndThrow(header);
  }

  if (header.requestID != txnID) {
    auto err = HgImporterError(
        "received unexpected transaction ID (",
        header.requestID,
        " != ",
        txnID,
        ") when reading ",
        cmdName,
        " response");
    XLOG(ERR) << err.what();
    throw err;
  }

  return header;
}

[[noreturn]] void HgImporter::readErrorAndThrow(const ChunkHeader& header) {
  auto buf = IOBuf{IOBuf::CREATE, header.dataLength};
  readFromHelper(buf.writableTail(), header.dataLength, "error response body");
  buf.append(header.dataLength);

  Cursor cursor(&buf);
  auto errorTypeLength = cursor.readBE<uint32_t>();
  StringPiece errorType{cursor.peekBytes().subpiece(0, errorTypeLength)};
  cursor.skip(errorTypeLength);
  auto messageLength = cursor.readBE<uint32_t>();
  StringPiece message{cursor.peekBytes().subpiece(0, messageLength)};
  cursor.skip(messageLength);

  XLOG(WARNING) << "error received from hg helper process: " << errorType
                << ": " << message;
  throw HgImportPyError(errorType, message);
}

HgImporter::TransactionID HgImporter::sendManifestRequest(
    folly::StringPiece revName) {
  stats_->getHgImporterStatsForCurrentThread().manifest.addValue(1);

  auto txnID = nextRequestID_++;
  ChunkHeader header;
  header.command = Endian::big<uint32_t>(CMD_MANIFEST);
  header.requestID = Endian::big<uint32_t>(txnID);
  header.flags = 0;
  header.dataLength = Endian::big<uint32_t>(folly::to_narrow(revName.size()));

  std::array<struct iovec, 2> iov;
  iov[0].iov_base = &header;
  iov[0].iov_len = sizeof(header);
  iov[1].iov_base = const_cast<char*>(revName.data());
  iov[1].iov_len = revName.size();
  writeToHelper(iov, "CMD_MANIFEST");

  return txnID;
}

HgImporter::TransactionID HgImporter::sendManifestNodeRequest(
    folly::StringPiece revName) {
  stats_->getHgImporterStatsForCurrentThread().manifestNodeForCommit.addValue(
      1);

  auto txnID = nextRequestID_++;
  ChunkHeader header;
  header.command = Endian::big<uint32_t>(CMD_MANIFEST_NODE_FOR_COMMIT);
  header.requestID = Endian::big<uint32_t>(txnID);
  header.flags = 0;
  header.dataLength = Endian::big<uint32_t>(folly::to_narrow(revName.size()));

  std::array<struct iovec, 2> iov;
  iov[0].iov_base = &header;
  iov[0].iov_len = sizeof(header);
  iov[1].iov_base = const_cast<char*>(revName.data());
  iov[1].iov_len = revName.size();
  writeToHelper(iov, "CMD_MANIFEST_NODE_FOR_COMMIT");

  return txnID;
}

HgImporter::TransactionID HgImporter::sendFileRequest(
    RelativePathPiece path,
    Hash revHash) {
  stats_->getHgImporterStatsForCurrentThread().catFile.addValue(1);

  auto txnID = nextRequestID_++;
  ChunkHeader header;
  header.command = Endian::big<uint32_t>(CMD_CAT_FILE);
  header.requestID = Endian::big<uint32_t>(txnID);
  header.flags = 0;
  StringPiece pathStr = path.stringPiece();
  header.dataLength =
      Endian::big<uint32_t>(folly::to_narrow(Hash::RAW_SIZE + pathStr.size()));

  std::array<struct iovec, 3> iov;
  iov[0].iov_base = &header;
  iov[0].iov_len = sizeof(header);
  iov[1].iov_base = const_cast<uint8_t*>(revHash.getBytes().data());
  iov[1].iov_len = Hash::RAW_SIZE;
  iov[2].iov_base = const_cast<char*>(pathStr.data());
  iov[2].iov_len = pathStr.size();
  writeToHelper(iov, "CMD_CAT_FILE");

  return txnID;
}

HgImporter::TransactionID HgImporter::sendPrefetchFilesRequest(
    const std::vector<HgProxyHash>& files) {
  stats_->getHgImporterStatsForCurrentThread().prefetchFiles.addValue(1);

  auto txnID = nextRequestID_++;
  ChunkHeader header;
  header.command = Endian::big<uint32_t>(CMD_PREFETCH_FILES);
  header.requestID = Endian::big<uint32_t>(txnID);
  header.flags = 0;

  // Compute the length of the body
  size_t dataLength = sizeof(uint32_t);
  for (const auto& file : files) {
    dataLength += sizeof(uint32_t) + file.path().stringPiece().size() +
        (Hash::RAW_SIZE * 2);
  }
  if (dataLength > std::numeric_limits<uint32_t>::max()) {
    throw std::runtime_error(
        folly::to<string>("prefetch files request is too large: ", dataLength));
  }
  header.dataLength = Endian::big<uint32_t>(folly::to_narrow(dataLength));

  // Serialize the body.
  // We serialize all of the filename lengths first, then all of the strings and
  // hashes later.  This is purely to make it easier to deserialize in python
  // using the struct module.
  //
  // The hashes are serialized as hex since that is how the python code needs
  // them.
  IOBuf buf(IOBuf::CREATE, dataLength);
  Appender appender(&buf, 0);
  appender.writeBE<uint32_t>(folly::to_narrow(files.size()));
  for (const auto& file : files) {
    auto fileName = file.path().stringPiece();
    appender.writeBE<uint32_t>(folly::to_narrow(fileName.size()));
  }
  for (const auto& file : files) {
    auto fileName = file.path().stringPiece();
    appender.push(fileName);
    // TODO: It would be nice to have a function that can hexlify the hash
    // data directly into the IOBuf without making a copy in a temporary string.
    // This isn't really that big of a deal though.
    appender.push(StringPiece(file.revHash().toString()));
  }
  DCHECK_EQ(buf.length(), dataLength);

  std::array<struct iovec, 2> iov;
  iov[0].iov_base = &header;
  iov[0].iov_len = sizeof(header);
  iov[1].iov_base = const_cast<uint8_t*>(buf.data());
  iov[1].iov_len = buf.length();
  writeToHelper(iov, "CMD_PREFETCH_FILES");

  return txnID;
}

HgImporter::TransactionID HgImporter::sendFetchTreeRequest(
    RelativePathPiece path,
    Hash pathManifestNode) {
  stats_->getHgImporterStatsForCurrentThread().fetchTree.addValue(1);

  auto txnID = nextRequestID_++;
  ChunkHeader header;
  header.command = Endian::big<uint32_t>(CMD_FETCH_TREE);
  header.requestID = Endian::big<uint32_t>(txnID);
  header.flags = 0;
  StringPiece pathStr = path.stringPiece();
  header.dataLength =
      Endian::big<uint32_t>(folly::to_narrow(Hash::RAW_SIZE + pathStr.size()));

  std::array<struct iovec, 3> iov;
  iov[0].iov_base = &header;
  iov[0].iov_len = sizeof(header);
  iov[1].iov_base = const_cast<uint8_t*>(pathManifestNode.getBytes().data());
  iov[1].iov_len = Hash::RAW_SIZE;
  iov[2].iov_base = const_cast<char*>(pathStr.data());
  iov[2].iov_len = pathStr.size();
  writeToHelper(iov, "CMD_FETCH_TREE");

  return txnID;
}

namespace {
std::string errStr() {
#ifndef _WIN32
  return folly::errnoStr(errno);
#else
  return win32ErrorToString(GetLastError());
#endif
}
} // namespace

void HgImporter::readFromHelper(void* buf, uint32_t size, StringPiece context) {
  size_t bytesRead;

#ifdef _WIN32
  auto result = Pipe::read(helperOut_, buf, size);
#else
  auto result = folly::readFull(helperOut_, buf, size);
#endif
  if (result < 0) {
    HgImporterError err(
        "error reading ", context, " from debugedenimporthelper: ", errStr());
    XLOG(ERR) << err.what();
    throw err;
  }
  bytesRead = static_cast<size_t>(result);
  if (bytesRead != size) {
    // The helper process closed the pipe early.
    // This generally means that it exited.
    HgImporterEofError err(
        "received unexpected EOF from debugedenimporthelper after ",
        bytesRead,
        " bytes while reading ",
        context);
    XLOG(ERR) << err.what();
    throw err;
  }
}

void HgImporter::writeToHelper(
    struct iovec* iov,
    size_t numIov,
    StringPiece context) {
#ifdef _WIN32
  auto result = Pipe::writevFull(helperIn_, iov, numIov);
#else
  auto result = folly::writevFull(helperIn_, iov, numIov);
#endif
  if (result < 0) {
    HgImporterError err(
        "error writing ", context, " to debugedenimporthelper: ", errStr());
    XLOG(ERR) << err.what();
    throw err;
  }
  // writevFull() will always write the full contents or fail, so we don't need
  // to check that the length written matches our input.
}

const ImporterOptions& HgImporter::getOptions() const {
  return options_;
}

HgImporterManager::HgImporterManager(
    AbsolutePathPiece repoPath,
    std::shared_ptr<EdenStats> stats,
    std::optional<AbsolutePath> importHelperScript)
    : repoPath_{repoPath},
      stats_{std::move(stats)},
      importHelperScript_{importHelperScript} {}

template <typename Fn>
auto HgImporterManager::retryOnError(Fn&& fn) {
  bool retried = false;

  auto retryableError = [this, &retried](const std::exception& ex) {
    resetHgImporter(ex);
    if (retried) {
      throw;
    } else {
      XLOG(INFO) << "restarting hg_import_helper and retrying operation";
      retried = true;
    }
  };

  while (true) {
    try {
      return fn(getImporter());
    } catch (const HgImportPyError& ex) {
      if (ex.errorType() == "ResetRepoError") {
        // The python code thinks its repository state has gone bad, and
        // is requesting to be restarted
        retryableError(ex);
      } else {
        throw;
      }
    } catch (const HgImporterError& ex) {
      retryableError(ex);
    }
  }
}

Hash HgImporterManager::resolveManifestNode(StringPiece revName) {
  return retryOnError([&](HgImporter* importer) {
    return importer->resolveManifestNode(revName);
  });
}

unique_ptr<Blob> HgImporterManager::importFileContents(
    RelativePathPiece path,
    Hash blobHash) {
  return retryOnError([=](HgImporter* importer) {
    return importer->importFileContents(path, blobHash);
  });
}

void HgImporterManager::prefetchFiles(const std::vector<HgProxyHash>& files) {
  return retryOnError(
      [&](HgImporter* importer) { return importer->prefetchFiles(files); });
}

void HgImporterManager::fetchTree(
    RelativePathPiece path,
    Hash pathManifestNode) {
  return retryOnError([&](HgImporter* importer) {
    return importer->fetchTree(path, pathManifestNode);
  });
}

HgImporter* HgImporterManager::getImporter() {
  if (!importer_) {
    importer_ = make_unique<HgImporter>(repoPath_, stats_, importHelperScript_);
  }
  return importer_.get();
}

void HgImporterManager::resetHgImporter(const std::exception& ex) {
  importer_.reset();
  XLOG(WARN) << "error communicating with debugedenimporthelper: " << ex.what();
}

} // namespace eden
} // namespace facebook
