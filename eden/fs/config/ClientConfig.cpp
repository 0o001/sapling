/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "ClientConfig.h"
#include <boost/algorithm/string.hpp>
#include <boost/filesystem.hpp>
#include <boost/range/adaptor/reversed.hpp>
#include <folly/File.h>
#include <folly/FileUtil.h>
#include <folly/String.h>
#include <folly/json.h>

using std::string;

namespace {
// INI config file
const facebook::eden::RelativePathPiece kLocalConfig{"edenrc"};

// Keys for the config INI file.
constexpr folly::StringPiece kBindMountsKey{"bindmounts "};
constexpr folly::StringPiece kRepositoryKey{"repository "};
constexpr folly::StringPiece kRepoNameKey{"repository.name"};
constexpr folly::StringPiece kRepoHooksKey{"hooks"};
constexpr folly::StringPiece kRepoTypeKey{"type"};
constexpr folly::StringPiece kRepoSourceKey{"path"};

// Files of interest in the client directory.
const facebook::eden::RelativePathPiece kSnapshotFile{"SNAPSHOT"};
const facebook::eden::RelativePathPiece kBindMountsDir{"bind-mounts"};
const facebook::eden::RelativePathPiece kCloneSuccessFile{"clone-succeeded"};
const facebook::eden::RelativePathPiece kOverlayDir{"local"};
const facebook::eden::RelativePathPiece kDirstateFile{"dirstate"};

// File holding mapping of client directories.
const facebook::eden::RelativePathPiece kClientDirectoryMap{"config.json"};
}

namespace facebook {
namespace eden {

using defaultPtree = boost::property_tree::basic_ptree<string, string>;

ClientConfig::ClientConfig(
    AbsolutePathPiece mountPath,
    AbsolutePathPiece clientDirectory)
    : clientDirectory_(clientDirectory), mountPath_(mountPath) {}

Hash ClientConfig::getSnapshotID() const {
  // Read the snapshot.
  auto snapshotFile = getSnapshotPath();
  string snapshotFileContents;
  folly::readFile(snapshotFile.c_str(), snapshotFileContents);
  // Make sure to remove any leading or trailing whitespace.
  auto snapshotID = folly::trimWhitespace(snapshotFileContents);
  return Hash{snapshotID};
}

void ClientConfig::setSnapshotID(Hash& id) const {
  auto snapshotPath = getSnapshotPath();
  auto hashStr = id.toString() + "\n";
  folly::writeFileAtomic(
      snapshotPath.stringPiece(), folly::StringPiece(hashStr));
}

AbsolutePath ClientConfig::getSnapshotPath() const {
  return clientDirectory_ + kSnapshotFile;
}

AbsolutePath ClientConfig::getOverlayPath() const {
  return clientDirectory_ + kOverlayDir;
}

AbsolutePath ClientConfig::getCloneSuccessPath() const {
  return clientDirectory_ + kCloneSuccessFile;
}

AbsolutePath ClientConfig::getDirstateStoragePath() const {
  return clientDirectory_ + kDirstateFile;
}

ClientConfig::ConfigData ClientConfig::loadConfigData(
    AbsolutePathPiece systemConfigDir,
    AbsolutePathPiece configPath) {
  ConfigData resultData;
  // Get global config files
  boost::filesystem::path rcDir(folly::to<string>(systemConfigDir));
  std::vector<string> rcFiles;
  if (boost::filesystem::is_directory(rcDir)) {
    for (auto it : boost::filesystem::directory_iterator(rcDir)) {
      rcFiles.push_back(it.path().string());
    }
  }
  sort(rcFiles.begin(), rcFiles.end());

  // Get home config file
  auto userConfigPath = AbsolutePath{configPath};
  rcFiles.push_back(userConfigPath.c_str());

  // Parse repository data in order to compile them
  for (auto rc : boost::adaptors::reverse(rcFiles)) {
    if (access(rc.c_str(), R_OK) != 0) {
      continue;
    }
    // Only add repository data from the first config file that references it
    ConfigData configData;
    boost::property_tree::ini_parser::read_ini(rc, configData);
    for (auto& entry : configData) {
      if (resultData.get_child(entry.first, defaultPtree()).empty()) {
        resultData.put_child(entry.first, entry.second);
      }
    }
  }
  return resultData;
}

std::unique_ptr<ClientConfig> ClientConfig::loadFromClientDirectory(
    AbsolutePathPiece mountPath,
    AbsolutePathPiece clientDirectory,
    const ConfigData* configData) {
  // Extract repository name from the client config file
  ConfigData repoData;
  boost::filesystem::path configFile(
      folly::to<string>(clientDirectory + kLocalConfig));
  boost::property_tree::ini_parser::read_ini(configFile.string(), repoData);
  auto repoName = repoData.get(kRepoNameKey.toString(), "");

  // Get the data of repository repoName from config files
  string repoHeader = kRepositoryKey.toString() + repoName;
  repoData = configData->get_child(repoHeader, defaultPtree());

  // Repository data not found
  if (repoData.empty()) {
    throw std::runtime_error("Could not find repository data for " + repoName);
  }

  // Construct ClientConfig object
  auto config = std::make_unique<ClientConfig>(mountPath, clientDirectory);

  // Extract the bind mounts
  string bindMountHeader = kBindMountsKey.toString() + repoName;
  auto bindMountPoints = configData->get_child(bindMountHeader, defaultPtree());
  AbsolutePath bindMountsPath = clientDirectory + kBindMountsDir;
  for (auto item : bindMountPoints) {
    auto pathInClientDir = bindMountsPath + RelativePathPiece{item.first};
    auto pathInMountDir = mountPath + RelativePathPiece{item.second.data()};
    config->bindMounts_.emplace_back(pathInClientDir, pathInMountDir);
  }

  // Load repository information
  config->repoType_ = repoData.get(kRepoTypeKey.toString(), "");
  config->repoSource_ = repoData.get(kRepoSourceKey.toString(), "");
  auto hooksPath = repoData.get(kRepoHooksKey.toString(), "");
  if (hooksPath != "") {
    config->repoHooks_ = AbsolutePath{hooksPath};
  }

  return config;
}

folly::dynamic ClientConfig::loadClientDirectoryMap(AbsolutePathPiece edenDir) {
  // Extract the JSON and strip any comments.
  std::string jsonContents;
  auto configJsonFile = edenDir + kClientDirectoryMap;
  folly::readFile(configJsonFile.c_str(), jsonContents);
  auto jsonWithoutComments = folly::json::stripComments(jsonContents);
  if (jsonWithoutComments.empty()) {
    return folly::dynamic::object();
  }

  // Parse the comment-free JSON while tolerating trailing commas.
  folly::json::serialization_opts options;
  options.allow_trailing_comma = true;
  return folly::parseJson(jsonWithoutComments, options);
}

AbsolutePathPiece ClientConfig::getRepoHooks() const {
  return repoHooks_.hasValue() ? repoHooks_.value()
                               : AbsolutePathPiece{"/etc/eden/hooks"};
}
}
}
