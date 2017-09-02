/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/model/Hash.h"

#include <folly/Optional.h>
#include <folly/experimental/TestUtil.h>
#include <folly/experimental/logging/Init.h>
#include <folly/experimental/logging/xlog.h>
#include <folly/init/Init.h>
#include <folly/io/Cursor.h>
#include <gflags/gflags.h>
#include <rocksdb/db.h>
#include <rocksdb/utilities/options_util.h>
#include <sysexits.h>

#include "eden/fs/model/Tree.h"
#include "eden/fs/store/LocalStore.h"
#include "eden/fs/store/hg/HgImporter.h"
#include "eden/fs/store/hg/HgManifestImporter.h"
#include "eden/fs/utils/PathFuncs.h"

DEFINE_string(edenDir, "", "The path to the .eden directory");
DEFINE_string(rev, "", "The revision ID to import");
DEFINE_string(logging, "eden=DBG2", "The logging configuration string");
DEFINE_string(
    import_type,
    "flat",
    "The hg import mechanism to use: \"flat\" or \"tree\"");
DEFINE_string(
    flat_import_file,
    "",
    "Import flat manifest data from a manifest dump in the specified file.");
DEFINE_string(
    subdir,
    "",
    "A subdirectory to import when using --import_type=tree.");
DEFINE_string(
    rocksdb_options_file,
    "",
    "A path to a rocksdb options file to use when creating a "
    "temporary rocksdb");
DEFINE_bool(
    tree_import_recurse,
    true,
    "Recursively import all trees under the specified subdirectory when "
    "performing a treemanifest import");

using namespace facebook::eden;
using folly::test::TemporaryDirectory;

using folly::Endian;
using folly::IOBuf;
using folly::io::Cursor;
using folly::StringPiece;
using std::string;

namespace {

std::unique_ptr<rocksdb::DB> createRocksDb(AbsolutePathPiece dbPath) {
  rocksdb::Options options;
  if (FLAGS_rocksdb_options_file.empty()) {
    options.IncreaseParallelism();
    options.OptimizeLevelStyleCompaction();
  } else {
    std::vector<rocksdb::ColumnFamilyDescriptor> cfDescs;
    auto env = rocksdb::Env::Default();
    auto status = rocksdb::LoadOptionsFromFile(
        FLAGS_rocksdb_options_file, env, &options, &cfDescs);
    if (!status.ok()) {
      throw std::runtime_error(
          folly::to<string>("Failed to load DB options: ", status.ToString()));
    }
    fprintf(
        stderr,
        "loaded rocksdb options from %s\n",
        FLAGS_rocksdb_options_file.c_str());
  }

  options.create_if_missing = true;

  // Open DB.
  rocksdb::DB* db;
  auto status = rocksdb::DB::Open(options, dbPath.stringPiece().str(), &db);
  if (!status.ok()) {
    throw std::runtime_error(
        folly::to<string>("Failed to open DB: ", status.ToString()));
  }

  return std::unique_ptr<rocksdb::DB>(db);
}

void importTreeRecursive(
    HgImporter* importer,
    RelativePathPiece path,
    const Tree* tree) {
  for (const auto& entry : tree->getTreeEntries()) {
    if (entry.getFileType() == FileType::DIRECTORY) {
      auto entryPath = path + entry.getName();
      std::unique_ptr<Tree> subtree;
      try {
        subtree = importer->importTree(entry.getHash());
      } catch (const std::exception& ex) {
        printf(
            "** error importing tree %s: %s\n",
            entryPath.stringPiece().str().c_str(),
            ex.what());
        continue;
      }
      printf(
          "  Recursively imported \"%s\"\n",
          entryPath.stringPiece().str().c_str());
      importTreeRecursive(importer, entryPath, subtree.get());
    }
  }
}

int importTree(
    LocalStore* store,
    AbsolutePathPiece repoPath,
    StringPiece revName,
    RelativePath subdir) {
  HgImporter importer(repoPath, store);

  printf(
      "Importing revision \"%s\" using tree manifest\n", revName.str().c_str());
  auto rootHash = importer.importTreeManifest(revName);
  printf("/: %s\n", rootHash.toString().c_str());

  auto tree = store->getTree(rootHash);
  for (const auto& component : subdir.components()) {
    auto entry = tree->getEntryPtr(component);
    if (!entry) {
      printf("%s: not found\n", component.stringPiece().str().c_str());
      return EX_DATAERR;
    }
    if (entry->getFileType() != FileType::DIRECTORY) {
      printf("%s: not a tree\n", component.stringPiece().str().c_str());
      return EX_DATAERR;
    }
    printf(
        "%s: %s\n",
        component.stringPiece().str().c_str(),
        entry->getHash().toString().c_str());
    tree = importer.importTree(entry->getHash());
  }

  if (FLAGS_tree_import_recurse) {
    importTreeRecursive(&importer, subdir, tree.get());
  }

  return EX_OK;
}
}

int main(int argc, char* argv[]) {
  folly::init(&argc, &argv);
  folly::initLoggingGlogStyle(FLAGS_logging, folly::LogLevel::INFO, false);

  if (argc != 2) {
    fprintf(stderr, "usage: hg_import <repository>\n");
    return EX_USAGE;
  }
  auto repoPath = realpath(argv[1]);

  folly::Optional<TemporaryDirectory> tmpDir;
  AbsolutePath rocksPath;
  if (FLAGS_edenDir.empty()) {
    tmpDir = TemporaryDirectory("eden_hg_tester");
    rocksPath = AbsolutePath{tmpDir->path().string()};
    createRocksDb(rocksPath);
  } else {
    if (!FLAGS_rocksdb_options_file.empty()) {
      fprintf(
          stderr,
          "error: --edenDir and --rocksdb_options_file are incompatible\n");
      return EX_USAGE;
    }
    rocksPath =
        canonicalPath(FLAGS_edenDir) + RelativePathPiece{"storage/rocks-db"};
  }

  std::string revName = FLAGS_rev;
  if (revName.empty()) {
    revName = ".";
  }

  LocalStore store(rocksPath);

  int returnCode = EX_OK;
  if (!FLAGS_flat_import_file.empty()) {
    folly::File inputFile(FLAGS_flat_import_file);
    HgImporter::importFlatManifest(inputFile.fd(), &store);
  } else if (FLAGS_import_type == "flat") {
    HgImporter importer(repoPath, &store);
    printf("Importing revision \"%s\" using flat manifest\n", revName.c_str());
    auto rootHash = importer.importFlatManifest(revName);
    printf("Imported root tree: %s\n", rootHash.toString().c_str());
  } else if (FLAGS_import_type == "tree") {
    RelativePath path{FLAGS_subdir};
    returnCode = importTree(&store, repoPath, revName, path);
  } else {
    fprintf(
        stderr,
        "error: unknown import type \"%s\"; must be \"flat\" or \"tree\"\n",
        FLAGS_import_type.c_str());
    return EX_USAGE;
  }

  return returnCode;
}
