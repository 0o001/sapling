/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once
#include <string>
#include "eden/fs/model/Hash.h"
#include "eden/fs/utils/PathFuncs.h"
#include "eden/fs/win/utils/Handle.h"
#include "eden/fs/win/utils/StringConv.h"
#include "folly/Range.h"
#include "folly/portability/IOVec.h"

namespace facebook {
namespace eden {
/*
 * Following is a traits class for File System handles with its handle value and
 * close function.
 */
struct FileHandleTraits {
  using Type = HANDLE;

  static Type invalidHandleValue() noexcept {
    return INVALID_HANDLE_VALUE;
  }
  static void close(Type handle) noexcept {
    CloseHandle(handle);
  }
};

using FileHandle = HandleBase<FileHandleTraits>;

/*
 * This readFile will read the bytesToRead number of bytes or the entire file,
 * which ever is shorter. The buffer should be atleast bytesToRead long.
 */
FOLLY_NODISCARD DWORD readFile(HANDLE handle, void* buffer, size_t bytesToRead);

/*
 * This writeFile will write the buffer to the file pointed by the handle.
 * The buffer should be atleast bytesToWrite long.
 */

void writeFile(const void* buffer, size_t size, const wchar_t* filePath);

/*
 * readFile() will read the entire file when the bytesToRead is not passed or
 * when the file is smaller than bytesToRead. Otherwise it will read the
 * bytesToRead in the container. It will resize the container to fit the size.
 * To get the number of bytes read use container.size(). This function will
 * throw on failure.
 */

template <typename Container>
void readFile(
    const wchar_t* filePath,
    Container& data,
    size_t bytesToRead = std::numeric_limits<size_t>::max()) {
  static_assert(
      sizeof(data[0]) == 1,
      "readFile: only containers with byte-sized elements accepted");

  FileHandle fileHandle{CreateFile(
      filePath,
      GENERIC_READ,
      FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
      nullptr,
      OPEN_EXISTING,
      FILE_ATTRIBUTE_NORMAL,
      nullptr)};

  if (!fileHandle) {
    throw makeWin32ErrorExplicit(
        GetLastError(),
        folly::sformat(
            "Unable to open the file {}", wideToMultibyteString(filePath)));
  }
  if (bytesToRead == std::numeric_limits<size_t>::max()) {
    //
    // bytesToRead == std::numeric_limits<size_t>::max() means read the entire
    // file.
    //

    LARGE_INTEGER fileSize;
    if (!GetFileSizeEx(fileHandle.get(), &fileSize)) {
      throw makeWin32ErrorExplicit(
          GetLastError(),
          folly::sformat(
              "Unable to get the file size {}",
              wideToMultibyteString(filePath)));
    }
    bytesToRead = fileSize.QuadPart;
  }
  data.resize(bytesToRead, 0);
  auto readBytes = readFile(fileHandle.get(), data.data(), bytesToRead);

  data.resize(readBytes);
}

template <typename Container>
inline void readFile(
    const char* filePath,
    Container& data,
    size_t bytesToRead = std::numeric_limits<size_t>::max()) {
  readFile(multibyteToWideString(filePath).c_str(), data, bytesToRead);
}
/*
 * This will write the data to the file. If the file doesn't exist it will
 * create the file. If it exists it will overwrite the file.
 */

template <typename Container>
inline void writeFile(const Container& data, const wchar_t* filePath) {
  static_assert(
      sizeof(data[0]) == 1,
      "writeFile: only containers with byte-sized elements accepted");
  writeFile(&data[0], data.size(), filePath);
}

template <typename Container>
inline void writeFile(const Container& data, const char* filePath) {
  writeFile(data, multibyteToWideString(filePath).c_str());
}

/*
 * writeFileAtomic only works with POSIX path for now.
 */

void writeFileAtomic(const wchar_t* filePath, const folly::ByteRange data);

inline void writeFileAtomic(const char* filePath, const folly::ByteRange data) {
  writeFileAtomic(multibyteToWideString(filePath).c_str(), data);
}

inline void writeFileAtomic(const char* filePath, const std::string& data) {
  writeFileAtomic(
      multibyteToWideString(filePath).c_str(), folly::StringPiece(data));
}

inline void writeFileAtomic(const wchar_t* filePath, const std::string& data) {
  writeFileAtomic(filePath, folly::StringPiece(data));
}

Hash getFileSha1(AbsolutePathPiece filePath);

} // namespace eden
} // namespace facebook
