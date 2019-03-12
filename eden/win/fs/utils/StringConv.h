/*
 *  Copyright (c) 2018-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */

#pragma once
#include "folly/portability/Windows.h"

#include <algorithm>
#include <cassert>
#include <memory>
#include <string>
#include "eden/win/fs/utils/WinError.h"

namespace facebook {
namespace eden {

// TODO: Move these functions to the better location.

// The paths we receive from FS and cli are Windows paths (Win path separator
// and UTF16). For now there will be two separate areas in our Windows code one
// which will use Windows strings and the other with (UTF8 + Unix path
// separator). The functions in stringconv will be responsible to do the
// conversion.

static std::string wcharToString(const wchar_t* wideCString) {
  //
  // Return empty string if wideCString is nullptr or an empty string. Empty
  // string is a common scenario. All the FS ops for the root with
  // come with relative path as empty string.
  //
  if ((!wideCString) || (wideCString[0] == L'\0')) {
    return "";
  }

  // To avoid extra copy or using max size buffers we should get the size first
  // and allocate the right size buffer.
  int size = WideCharToMultiByte(CP_UTF8, 0, wideCString, -1, nullptr, 0, 0, 0);

  if (size > 0) {
    std::string multiByteString(size - 1, 0);
    size = WideCharToMultiByte(
        CP_UTF8, 0, wideCString, -1, multiByteString.data(), size, 0, 0);
    if (size > 0) {
      return multiByteString;
    }
  }
  throw makeWin32ErrorExplicit(
      GetLastError(), "Failed to convert wide char to char");
}

static std::wstring charToWstring(const char* multiByteCString) {
  //
  // Return empty string if multiByteCString is nullptr or an empty string.
  // Empty string is a common scenario. All the FS ops for the root
  // with come with relative path as empty string.
  //
  if ((!multiByteCString) || (multiByteCString[0] == '\0')) {
    return L"";
  }

  // To avoid extra copy or using max size buffers we should get the size first
  // and allocate the right size buffer.
  int size = MultiByteToWideChar(CP_UTF8, 0, multiByteCString, -1, nullptr, 0);

  if (size > 0) {
    std::wstring wideString(size - 1, 0);
    size = MultiByteToWideChar(
        CP_UTF8, 0, multiByteCString, -1, wideString.data(), size);
    if (size > 0) {
      return wideString;
    }
  }
  throw makeWin32ErrorExplicit(
      GetLastError(), "Failed to convert char to wide char");
}

static std::string wstringToString(const std::wstring& wideString) {
  return wcharToString(wideString.c_str());
}

static std::wstring stringToWstring(const std::string& multiByteString) {
  return charToWstring(multiByteString.c_str());
}

static std::string winToEdenPath(const std::wstring& winString) {
  std::string edenStr = wstringToString(winString);
#ifndef USE_WIN_PATH_SEPERATOR
  std::replace(edenStr.begin(), edenStr.end(), '\\', '/');
#endif
  return edenStr;
}

static std::wstring edenToWinPath(const std::string& edenString) {
  std::wstring winStr = stringToWstring(edenString);
#ifndef USE_WIN_PATH_SEPERATOR
  std::replace(winStr.begin(), winStr.end(), L'/', L'\\');
#endif
  return winStr;
}

static std::string winToEdenName(const std::wstring& wideName) {
  //
  // This function is to convert the final name component of the path
  // which should not contain the path delimiter. Assert that.
  //
  assert(wideName.find(L'\\') == std::wstring::npos);
  return wstringToString(wideName.c_str());
}

static std::wstring edenToWinName(const std::string& name) {
  //
  // This function is to convert the final name component of the path
  // which should not contain the path delimiter. Assert that.
  //
  assert(name.find('/') == std::string::npos);
  return stringToWstring(name.c_str());
}

} // namespace eden
} // namespace facebook
