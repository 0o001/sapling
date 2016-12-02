/*
 *  Copyright (c) 2016, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#pragma once

#include <boost/property_tree/ini_parser.hpp>
#include <folly/Optional.h>
#include <folly/dynamic.h>
#include "eden/fs/model/Hash.h"
#include "eden/utils/PathFuncs.h"

namespace facebook {
namespace eden {

struct BindMount {
  BindMount(AbsolutePathPiece clientDirPath, AbsolutePathPiece mountDirPath)
      : pathInClientDir(clientDirPath), pathInMountDir(mountDirPath) {}

  bool operator==(const BindMount& other) const {
    return pathInClientDir == other.pathInClientDir &&
        pathInMountDir == other.pathInMountDir;
  }

  AbsolutePath pathInClientDir;
  AbsolutePath pathInMountDir;
};

inline void operator<<(std::ostream& out, const BindMount& bindMount) {
  out << "BindMount{pathInClientDir=" << bindMount.pathInClientDir
      << "; pathInMountDir=" << bindMount.pathInMountDir << "}";
}

class ClientConfig {
 public:
  using ConfigData = boost::property_tree::ptree;

  /**
   * Manually construct a ClientConfig object.
   *
   * Note that most callers will probably want to use the
   * loadFromClientDirectory() factory function to create a ClientConfig object
   * from an existing client directory, rather than directly calling this
   * constructor.
   */
  ClientConfig(AbsolutePathPiece mountPath, AbsolutePathPiece clientDirectory);

  /**
   * Load a ClientConfig object from the edenrc file in a client directory.
   *
   * @param mountPath  The path where the client is (or will be) mounted.
   * @param clientDirectory  The eden client data directory, where the client
   *     configuration file can be found (along with its overlay and other
   *     data).
   * @param configData  The eden server configuration data.  (This is the
   *     global server configuration rather than the client-specific config
   *     data.  This function will load the client-specific config data from
   *     the clientDirectory.)
   */
  static std::unique_ptr<ClientConfig> loadFromClientDirectory(
      AbsolutePathPiece mountPath,
      AbsolutePathPiece clientDirectory,
      const ConfigData* configData);

  /**
   * Load the global server configuration data.
   */
  static ConfigData loadConfigData(
      AbsolutePathPiece systemConfigDir,
      AbsolutePathPiece homeDirectory);

  static folly::dynamic loadClientDirectoryMap(AbsolutePathPiece edenDir);

  Hash getSnapshotID() const;

  const AbsolutePath& getMountPath() const {
    return mountPath_;
  }

  /** @return Path to the directory where overlay information is stored. */
  AbsolutePath getOverlayPath() const;

  const std::vector<BindMount>& getBindMounts() const {
    return bindMounts_;
  }

  /**
   * Get the repository type.
   *
   * Currently supported types include "git" and "hg".
   */
  const std::string& getRepoType() const {
    return repoType_;
  }

  /**
   * Get the repository source.
   *
   * The meaning and format of repository source string depends on the
   * repository type.  For git and hg repositories, this is the path to the
   * git or mercuial repository.
   */
  const std::string& getRepoSource() const {
    return repoSource_;
  }

  /** Path to the directory where the scripts for the hooks are defined. */
  AbsolutePathPiece getRepoHooks() const;

  /** File that will be written once the clone for this client has succeeded. */
  AbsolutePath getCloneSuccessPath() const;

  /** Path to the file where the dirstate data is stored. */
  AbsolutePath getDirstateStoragePath() const;

 private:
  ClientConfig(
      AbsolutePathPiece clientDirectory,
      AbsolutePathPiece mountPath,
      std::vector<BindMount>&& bindMounts);

  AbsolutePath clientDirectory_;
  AbsolutePath mountPath_;
  std::vector<BindMount> bindMounts_;
  std::string repoType_;
  std::string repoSource_;
  folly::Optional<AbsolutePath> repoHooks_;
};
}
}
