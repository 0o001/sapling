/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#ifndef _WIN32

#include "eden/fs/inodes/NfsDispatcherImpl.h"

#include <folly/futures/Future.h>
#include "eden/fs/inodes/EdenMount.h"
#include "eden/fs/inodes/FileInode.h"
#include "eden/fs/inodes/InodeBase.h"
#include "eden/fs/inodes/InodeMap.h"
#include "eden/fs/inodes/TreeInode.h"

namespace facebook::eden {

NfsDispatcherImpl::NfsDispatcherImpl(EdenMount* mount)
    : NfsDispatcher(mount->getStats(), mount->getClock()),
      mount_(mount),
      inodeMap_(mount_->getInodeMap()) {}

folly::Future<struct stat> NfsDispatcherImpl::getattr(
    InodeNumber ino,
    ObjectFetchContext& context) {
  return inodeMap_->lookupInode(ino).thenValue(
      [&context](const InodePtr& inode) { return inode->stat(context); });
}

folly::Future<NfsDispatcher::SetattrRes> NfsDispatcherImpl::setattr(
    InodeNumber ino,
    DesiredMetadata desired,
    ObjectFetchContext& /*context*/) {
  return inodeMap_->lookupInode(ino)
      .thenValue([desired = std::move(desired)](const InodePtr& inode) {
        // TODO(xavierd): Modify setattr to obtain pre stat of the file.
        return inode->setattr(desired);
      })
      .thenValue([](struct stat st) {
        return NfsDispatcher::SetattrRes{std::nullopt, st};
      });
}

folly::Future<InodeNumber> NfsDispatcherImpl::getParent(
    InodeNumber ino,
    ObjectFetchContext& /*context*/) {
  return inodeMap_->lookupTreeInode(ino).thenValue(
      [](const TreeInodePtr& inode) {
        return inode->getParentRacy()->getNodeId();
      });
}

folly::Future<std::tuple<InodeNumber, struct stat>> NfsDispatcherImpl::lookup(
    InodeNumber dir,
    PathComponent name,
    ObjectFetchContext& context) {
  return inodeMap_->lookupTreeInode(dir)
      .thenValue([name = std::move(name), &context](const TreeInodePtr& inode) {
        return inode->getOrLoadChild(name, context);
      })
      .thenValue([&context](InodePtr&& inode) {
        auto statFut = inode->stat(context);
        return std::move(statFut).thenValue(
            [inode = std::move(inode)](
                struct stat stat) -> std::tuple<InodeNumber, struct stat> {
              inode->incFsRefcount();
              return {inode->getNodeId(), stat};
            });
      });
}

folly::Future<std::string> NfsDispatcherImpl::readlink(
    InodeNumber ino,
    ObjectFetchContext& context) {
  return inodeMap_->lookupFileInode(ino).thenValue(
      [&context](const FileInodePtr& inode) {
        return inode->readlink(context);
      });
}

folly::Future<NfsDispatcher::ReadRes> NfsDispatcherImpl::read(
    InodeNumber ino,
    size_t size,
    off_t offset,
    ObjectFetchContext& context) {
  return inodeMap_->lookupFileInode(ino).thenValue(
      [&context, size, offset](const FileInodePtr& inode) {
        return inode->read(size, offset, context)
            .thenValue([size](std::unique_ptr<folly::IOBuf>&& data) {
              // TODO(xavierd): Detect an empty file when a empty read is
              // performed. This forces the client to issue 2 reads: one to
              // read the file, and the second to validate it is at the end of
              // the file. If we could detect an EOF without the second read we
              // can half the number of READ RPC.
              auto isEof = size != 0 && data->empty();
              return ReadRes{std::move(data), isEof};
            });
      });
}

folly::Future<NfsDispatcher::WriteRes> NfsDispatcherImpl::write(
    InodeNumber ino,
    std::unique_ptr<folly::IOBuf> data,
    off_t offset,
    ObjectFetchContext& /*context*/) {
  return inodeMap_->lookupFileInode(ino).thenValue(
      [data = std::move(data), offset](const FileInodePtr& inode) mutable {
        // TODO(xavierd): Modify write to obtain pre and post stat of the file.
        return inode->write(std::move(data), offset)
            .thenValue([](size_t written) {
              return WriteRes{written, std::nullopt, std::nullopt};
            });
      });
}

folly::Future<NfsDispatcher::CreateRes> NfsDispatcherImpl::create(
    InodeNumber dir,
    PathComponent name,
    mode_t mode,
    ObjectFetchContext& context) {
  // Make sure that we're attempting to create a file.
  mode = S_IFREG | (0777 & mode);
  return inodeMap_->lookupTreeInode(dir).thenValue(
      [&context, name = std::move(name), mode](const TreeInodePtr& inode) {
        // TODO(xavierd): Modify mknod to obtain the pre and post stat of the
        // directory.
        // Set dev to 0 as this is unused for a regular file.
        auto newFile = inode->mknod(name, mode, 0, InvalidationRequired::No);
        auto statFut = newFile->stat(context);
        return std::move(statFut).thenValue(
            [newFile = std::move(newFile)](struct stat&& stat) {
              newFile->incFsRefcount();
              return CreateRes{
                  newFile->getNodeId(),
                  std::move(stat),
                  std::nullopt,
                  std::nullopt};
            });
      });
}

folly::Future<NfsDispatcher::MkdirRes> NfsDispatcherImpl::mkdir(
    InodeNumber dir,
    PathComponent name,
    mode_t mode,
    ObjectFetchContext& context) {
  return inodeMap_->lookupTreeInode(dir).thenValue(
      [&context, name = std::move(name), mode](const TreeInodePtr& inode) {
        // TODO(xavierd): Modify mkdir to obtain the pre and post stat of the
        // directory.
        auto newDir = inode->mkdir(name, mode, InvalidationRequired::No);
        auto statFut = newDir->stat(context);
        return std::move(statFut).thenValue([newDir = std::move(newDir)](
                                                struct stat&& stat) {
          newDir->incFsRefcount();
          return MkdirRes{
              newDir->getNodeId(), std::move(stat), std::nullopt, std::nullopt};
        });
      });
}

folly::Future<NfsDispatcher::SymlinkRes> NfsDispatcherImpl::symlink(
    InodeNumber dir,
    PathComponent name,
    std::string data,
    ObjectFetchContext& context) {
  return inodeMap_->lookupTreeInode(dir).thenValue(
      [&context, name = std::move(name), data = std::move(data)](
          const TreeInodePtr& inode) {
        // TODO(xavierd): Modify symlink to obtain the pre and post stat of the
        // directory.
        auto symlink = inode->symlink(name, data, InvalidationRequired::No);
        auto statFut = symlink->stat(context);
        return std::move(statFut).thenValue(
            [symlink = std::move(symlink)](struct stat&& stat) {
              symlink->incFsRefcount();
              return SymlinkRes{
                  symlink->getNodeId(),
                  std::move(stat),
                  std::nullopt,
                  std::nullopt};
            });
      });
}

folly::Future<NfsDispatcher::UnlinkRes> NfsDispatcherImpl::unlink(
    InodeNumber dir,
    PathComponent name,
    ObjectFetchContext& context) {
  return inodeMap_->lookupTreeInode(dir).thenValue(
      [&context, name = std::move(name)](const TreeInodePtr& inode) {
        return inode->unlink(name, InvalidationRequired::No, context)
            .thenValue([](auto&&) {
              // TODO(xavierd): Modify unlink to obtain the pre and post stat
              // of the directory.
              return NfsDispatcher::UnlinkRes{std::nullopt, std::nullopt};
            });
      });
}

folly::Future<NfsDispatcher::RmdirRes> NfsDispatcherImpl::rmdir(
    InodeNumber dir,
    PathComponent name,
    ObjectFetchContext& context) {
  return inodeMap_->lookupTreeInode(dir).thenValue(
      [&context, name = std::move(name)](const TreeInodePtr& inode) {
        return inode->rmdir(name, InvalidationRequired::No, context)
            .thenValue([](auto&&) {
              // TODO(xavierd): Modify rmdir to obtain the pre and post stat of
              // the directory.
              return NfsDispatcher::RmdirRes{std::nullopt, std::nullopt};
            });
      });
}

folly::Future<NfsDispatcher::RenameRes> NfsDispatcherImpl::rename(
    InodeNumber fromIno,
    PathComponent fromName,
    InodeNumber toIno,
    PathComponent toName,
    ObjectFetchContext& /*context*/) {
  auto fromDir = inodeMap_->lookupTreeInode(fromIno);
  return inodeMap_->lookupTreeInode(toIno)
      .thenValue(
          [fromDir = std::move(fromDir),
           fromName = std::move(fromName),
           toName = std::move(toName)](TreeInodePtr&& toDirInode) mutable {
            return std::move(fromDir).thenValue(
                [fromName = std::move(fromName),
                 toName = std::move(toName),
                 toDirInode =
                     std::move(toDirInode)](const TreeInodePtr& fromDirInode) {
                  return fromDirInode->rename(
                      fromName, toDirInode, toName, InvalidationRequired::No);
                });
          })
      .thenValue([](auto&&) {
        // TODO(xavierd): collect pre and post dir stats.
        return NfsDispatcher::RenameRes{};
      });
}

folly::Future<NfsDispatcher::ReaddirRes> NfsDispatcherImpl::readdir(
    InodeNumber dir,
    off_t offset,
    uint32_t count,
    ObjectFetchContext& context) {
  return inodeMap_->lookupTreeInode(dir).thenValue(
      [&context, offset, count](const TreeInodePtr& inode) {
        auto [dirList, isEof] =
            inode->nfsReaddir(NfsDirList{count}, offset, context);
        return ReaddirRes{std::move(dirList), isEof};
      });
}

folly::Future<struct statfs> NfsDispatcherImpl::statfs(
    InodeNumber /*dir*/,
    ObjectFetchContext& /*context*/) {
  // See the comment in FuseDispatcherImpl::statfs for why we gather the statFs
  // from the overlay.
  return mount_->getOverlay()->statFs();
}

} // namespace facebook::eden

#endif
