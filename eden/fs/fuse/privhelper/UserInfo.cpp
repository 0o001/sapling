/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/fuse/privhelper/UserInfo.h"

#include <grp.h>
#include <pwd.h>
#include <vector>

#include <folly/Exception.h>

using folly::checkUnixError;
using folly::throwSystemError;

namespace facebook {
namespace eden {

struct UserInfo::PasswdEntry {
  struct passwd pwd;
  std::vector<char> buf;
};

void UserInfo::dropPrivileges() {
  // Configure the correct supplementary groups
  auto rc = initgroups(username_.c_str(), gid_);
  checkUnixError(rc, "failed to set supplementary groups");
  // Drop to the correct primary group
  rc = setregid(gid_, gid_);
  checkUnixError(rc, "failed to drop group privileges");
  // Drop to the correct user ID
  rc = setreuid(uid_, uid_);
  checkUnixError(rc, "failed to drop user privileges");
}

UserInfo::PasswdEntry UserInfo::getPasswdUid(uid_t uid) {
  static constexpr size_t initialBufSize = 1024;
  static constexpr size_t maxBufSize = 8192;
  PasswdEntry pwd;
  pwd.buf.resize(initialBufSize);

  struct passwd* result;
  while (true) {
    auto errnum =
        getpwuid_r(uid, &pwd.pwd, pwd.buf.data(), pwd.buf.size(), &result);
    if (errnum == 0) {
      break;
    } else if (errnum == ERANGE && pwd.buf.size() < maxBufSize) {
      // Retry with a bigger buffer
      pwd.buf.resize(pwd.buf.size() * 2);
      continue;
    } else {
      throwSystemError("unable to look up user information for UID ", uid);
    }
  }
  if (result == nullptr) {
    // No user info present for this UID.
    throwSystemError("no passwd entry found for UID ", uid);
  }

  return pwd;
}

bool UserInfo::initFromSudo() {
  // If SUDO_UID is not set, return false indicating we could not
  // find sudo-based identity information.
  auto sudoUid = getenv("SUDO_UID");
  if (sudoUid == nullptr) {
    return false;
  }

  // Throw an exception if SUDO_GID or SUDI_USER is not set, or if we cannot
  // parse them below.  We want to fail hard if we have SUDO_UID but we can't
  // use it for some reason.  We don't want to fall back to running as root in
  // this case.
  auto sudoGid = getenv("SUDO_GID");
  if (sudoGid == nullptr) {
    throw std::runtime_error("SUDO_UID set without SUDO_GID");
  }
  auto sudoUser = getenv("SUDO_USER");
  if (sudoUser == nullptr) {
    throw std::runtime_error("SUDO_UID set without SUDO_USER");
  }

  try {
    uid_ = folly::to<uid_t>(sudoUid);
  } catch (const std::range_error& ex) {
    throw std::runtime_error(
        std::string{"invalid value for SUDO_UID: "} + sudoUid);
  }
  try {
    gid_ = folly::to<gid_t>(sudoGid);
  } catch (const std::range_error& ex) {
    throw std::runtime_error(
        std::string{"invalid value for SUDO_GID: "} + sudoGid);
  }

  username_ = sudoUser;
  initHomedir();
  return true;
}

void UserInfo::initFromNonRoot(uid_t uid) {
  uid_ = uid;
  gid_ = getgid();

  // Always look up the username from the UID.
  // We cannot trust the USER environment variable--the user could have set
  // it to anything.
  auto pwd = getPasswdUid(uid_);
  username_ = pwd.pwd.pw_name;

  initHomedir(&pwd);
}

void UserInfo::initHomedir(PasswdEntry* pwd) {
  // We do trust the $HOME environment variable if it is set.
  // This does not need to be distrusted for security reasons--we can use any
  // arbitrary directory the user wants as long as they have read/write access
  // to it.  We only access it after dropping privileges.
  //
  // Note that we intentionally use canonicalPath() rather than realpath()
  // here.  realpath() will perform symlink resolution.  initHomedir() will
  // generally be run before we have dropped privileges, and we do not want to
  // try traversing symlinks that the user may not actually have permissions to
  // resolve.
  auto homeEnv = getenv("HOME");
  if (homeEnv != nullptr) {
    homeDirectory_ = canonicalPath(homeEnv);
    return;
  }

  PasswdEntry locallyLookedUp;
  if (!pwd) {
    locallyLookedUp = getPasswdUid(uid_);
    pwd = &locallyLookedUp;
  }

  if (pwd && pwd->pwd.pw_dir) {
    homeDirectory_ = canonicalPath(pwd->pwd.pw_dir);
    return;
  }

  // Fall back to the root directory if all else fails
  homeDirectory_ = AbsolutePath{"/"};
}

UserInfo UserInfo::lookup() {
  UserInfo info;
  // First check the real UID.  If it is non-root, use that.
  // This happens if our binary is setuid root and invoked by a non-root user.
  auto uid = getuid();
  if (uid != 0) {
    info.initFromNonRoot(uid);
    return info;
  }

  // If we are still here, our real UID is 0.
  // Check the SUDO_* environment variables in case we are running under sudo.
  if (info.initFromSudo()) {
    return info;
  }

  // If we are still here, we are actually running as root and could not find
  // non-root privileges to drop to.
  info.uid_ = uid;
  info.gid_ = getgid();
  auto pwd = getPasswdUid(info.uid_);
  info.username_ = pwd.pwd.pw_name;
  info.initHomedir(&pwd);
  return info;
}
} // namespace eden
} // namespace facebook
