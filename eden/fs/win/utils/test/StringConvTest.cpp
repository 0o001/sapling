/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/win/utils/StringConv.h"
#include <string>
#include "gtest/gtest.h"
using namespace facebook::eden;

TEST(StringConvTest, testWintoEdenPath) {
  std::wstring winPath = L"C:\\winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "C:/winPath/PATH1/path/File.txt";
  EXPECT_EQ(winToEdenPath(winPath), edenPath);
}

TEST(StringConvTest, testEdenToWinPath) {
  std::wstring winPath = L"C:\\winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "C:/winPath/PATH1/path/File.txt";

  EXPECT_EQ(edenToWinPath(edenPath), winPath);
}

TEST(StringConvTest, testWintoEdenPathWithEmptyString) {
  std::wstring winPath = L"";
  std::string edenPath = "";

  EXPECT_EQ(winToEdenPath(winPath), edenPath);
}

TEST(StringConvTest, testEdenToWinPathWithEmptyString) {
  std::wstring winPath = L"";
  std::string edenPath = "";

  EXPECT_EQ(edenToWinPath(edenPath), winPath);
}

TEST(StringConvTest, testWintoEdenPathWithLongString) {
  std::wstring winPath =
      L"C:\\winPath\\PATHaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\path\\File.txt";
  std::string edenPath =
      "C:/winPath/PATHaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaa/path/File.txt";

  EXPECT_EQ(winToEdenPath(winPath), edenPath);
}

TEST(StringConvTest, testEdenToWinPathWithLongString) {
  std::wstring winPath =
      L"C:\\winPath\\PATHaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\path\\File.txt";
  std::string edenPath =
      "C:/winPath/PATHaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      "aaaaaaaaaaaaaaaaaaaaaaaaaaaaa/path/File.txt";

  EXPECT_EQ(edenToWinPath(edenPath), winPath);
}

TEST(StringConvTest, testWintoEdenPathComponent) {
  std::wstring winPath = L"LongFileName.txt";
  std::string edenPath = "LongFileName.txt";

  EXPECT_EQ(winToEdenName(winPath), edenPath);
}

TEST(StringConvTest, testEdenToWinPathComponent) {
  std::wstring winPath = L"LongFileName.txt";
  std::string edenPath = "LongFileName.txt";

  EXPECT_EQ(edenToWinName(edenPath), winPath);
}

TEST(StringConvTest, testWinToEdenToWinPathRoundTrips) {
  std::wstring winPath = L"\\winPath\\PATH1\\path\\File.txt";
  std::string edenPath = winToEdenPath(winPath);
  std::wstring newWinPath = edenToWinPath(edenPath);
  EXPECT_EQ(winPath, newWinPath);
}

TEST(StringConvTest, testEdenToWinToEdenPathRoundTrips) {
  std::string edenPath = "/winPath/PATH1/path/File.txt";
  std::wstring winPath = edenToWinPath(edenPath);
  std::string newedenPath = winToEdenPath(winPath);
  EXPECT_EQ(newedenPath, edenPath);
}

TEST(StringConvTest, testWstringToString) {
  std::wstring wideStr = L"C:\\winPath\\PATH1\\path\\File.txt";
  std::string str = "C:\\winPath\\PATH1\\path\\File.txt";

  EXPECT_EQ(wideToMultibyteString(wideStr), str);
}

TEST(StringConvTest, testStringToWstring) {
  std::wstring wideStr = L"C:\\winPath\\PATH1\\path\\File.txt";
  std::string str = "C:\\winPath\\PATH1\\path\\File.txt";

  EXPECT_EQ(multibyteToWideString(str), wideStr);
}

TEST(StringConvTest, testWcharToString) {
  std::wstring wideStr = L"C:\\winPath\\PATH1\\path\\File.txt";
  std::string str = "C:\\winPath\\PATH1\\path\\File.txt";

  EXPECT_EQ(wideToMultibyteString(wideStr.c_str()), str);
}

TEST(StringConvTest, testcharToWstring) {
  std::wstring wideStr = L"C:\\winPath\\PATH1\\path\\File.txt";
  std::string str = "C:\\winPath\\PATH1\\path\\File.txt";

  EXPECT_EQ(multibyteToWideString(str.c_str()), wideStr);
}

TEST(StringConvTest, testWcharToStringWithNullptr) {
  std::string str = "";
  const wchar_t* wideStr = nullptr;

  EXPECT_EQ(wideToMultibyteString(wideStr), str);
}

TEST(StringConvTest, testcharToWstringWithNullptr) {
  std::wstring wideStr = L"";
  const char* str = nullptr;

  std::wstring newWStr = multibyteToWideString(str);
  EXPECT_EQ(newWStr, wideStr);
}

TEST(StringConvTest, testWcharToStringWithEmptyPath) {
  std::wstring wideStr = L"";
  std::string str = "";

  std::string newStr = wideToMultibyteString(wideStr.c_str());
  EXPECT_EQ(newStr, str);
}

TEST(StringConvTest, testcharToWstringWithEmptyPath) {
  std::wstring wideStr = L"";
  std::string str = "";

  std::wstring newWStr = multibyteToWideString(str.c_str());
  EXPECT_EQ(newWStr, wideStr);
}

TEST(StringConvTest, testWintoEdenPathRelativePath) {
  std::wstring winPath = L"winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "winPath/PATH1/path/File.txt";
  EXPECT_EQ(winToEdenPath(winPath), edenPath);
}

TEST(StringConvTest, testEdenToWinPathRelativePath) {
  std::wstring winPath = L"winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "winPath/PATH1/path/File.txt";

  EXPECT_EQ(edenToWinPath(edenPath), winPath);
}

TEST(StringConvTest, testWintoEdenPathMixedPath) {
  std::wstring winPath = L"mixed/winPath\\PATH1/path\\File.txt";
  std::string edenPath = "mixed/winPath/PATH1/path/File.txt";
  EXPECT_EQ(winToEdenPath(winPath), edenPath);
}

TEST(StringConvTest, testEdenToWinPathMixedPath) {
  std::wstring winPath = L"winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "winPath/PATH1\\path/File.txt";

  EXPECT_EQ(edenToWinPath(edenPath), winPath);
}

TEST(StringConvTest, testWintoEdenPathNTPath) {
  std::wstring winPath = L"\\??\\mixed\\winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "/??/mixed/winPath/PATH1/path/File.txt";

  EXPECT_EQ(winToEdenPath(winPath), edenPath);
}

TEST(StringConvTest, testEdenToWinPathNTPath) {
  std::wstring winPath = L"\\??\\mixed\\winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "/??/mixed/winPath/PATH1/path/File.txt";

  EXPECT_EQ(edenToWinPath(edenPath), winPath);
}

TEST(StringConvTest, testPieceToWString) {
  std::wstring widePath = L"/??/mixed/winPath/PATH1/path/File.txt";
  folly::StringPiece piece = "/??/mixed/winPath/PATH1/path/File.txt";

  EXPECT_EQ(widePath, multibyteToWideString(piece));
}

TEST(StringConvTest, testViewToWString) {
  std::wstring widePath = L"/??/mixed/winPath/PATH1/path/File.txt";
  std::string_view piece = "/??/mixed/winPath/PATH1/path/File.txt";

  EXPECT_EQ(widePath, multibyteToWideString(piece));
}

TEST(StringConvTest, testWViewToString) {
  std::wstring_view widePath = L"/??/mixed/winPath/PATH1/path/File.txt";
  std::string multiBytePath = "/??/mixed/winPath/PATH1/path/File.txt";

  EXPECT_EQ(multiBytePath, wideToMultibyteString(widePath));
}

TEST(StringConvTest, teststdPathToString) {
  std::filesystem::path widePath{
      L"\\??\\mixed\\winPath\\PATH1\\path\\File.txt"};
  std::string multiBytePath = "/??/mixed/winPath/PATH1/path/File.txt";

  EXPECT_EQ(multiBytePath, winToEdenPath(widePath));
}

TEST(StringConvTest, testWintoEdenPathRelativePathCStr) {
  std::wstring winPath = L"winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "winPath/PATH1/path/File.txt";
  EXPECT_EQ(winToEdenPath(winPath.c_str()), edenPath);
}

TEST(StringConvTest, testEdenToWinPathRelativePathCStr) {
  std::wstring winPath = L"winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "winPath/PATH1/path/File.txt";

  EXPECT_EQ(edenToWinPath(edenPath.c_str()), winPath);
}

TEST(StringConvTest, testWintoEdenPathMixedPathCStr) {
  std::wstring winPath = L"mixed/winPath\\PATH1/path\\File.txt";
  std::string edenPath = "mixed/winPath/PATH1/path/File.txt";
  EXPECT_EQ(winToEdenPath(winPath.c_str()), edenPath);
}

TEST(StringConvTest, testEdenToWinPathMixedPathCStr) {
  std::wstring winPath = L"winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "winPath/PATH1\\path/File.txt";

  EXPECT_EQ(edenToWinPath(edenPath.c_str()), winPath);
}

TEST(StringConvTest, testWintoEdenPathNTPathCStr) {
  std::wstring winPath = L"\\??\\mixed\\winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "/??/mixed/winPath/PATH1/path/File.txt";

  EXPECT_EQ(winToEdenPath(winPath.c_str()), edenPath);
}

TEST(StringConvTest, testEdenToWinPathNTPathCStr) {
  std::wstring winPath = L"\\??\\mixed\\winPath\\PATH1\\path\\File.txt";
  std::string edenPath = "/??/mixed/winPath/PATH1/path/File.txt";

  EXPECT_EQ(edenToWinPath(edenPath.c_str()), winPath);
}

TEST(StringConvTest, testWideToFbSting) {
  std::wstring winPath = L"mixed/winPath/PATH1/path/File.txt";
  folly::fbstring edenPath = "mixed/winPath/PATH1/path/File.txt";
  EXPECT_EQ(wideTofbString(winPath), edenPath);
}

TEST(StringConvTest, testWideToFbStingCstr) {
  std::wstring winPath = L"mixed/winPath/PATH1/path/File.txt";
  folly::fbstring edenPath = "mixed/winPath/PATH1/path/File.txt";
  EXPECT_EQ(wideTofbString(winPath.c_str()), edenPath);
}

TEST(StringConvTest, testWideCharToEdenPathComponent) {
  std::wstring winPath = L"FileName.txt";
  PathComponent edenPath{"FileName.txt"_pc};
  EXPECT_EQ(wideCharToEdenPathComponent(winPath), edenPath);
}

TEST(StringConvTest, testWideCharToEdenPathComponentCstr) {
  std::wstring winPath = L"FileName.txt";
  PathComponent edenPath{"FileName.txt"_pc};
  EXPECT_EQ(wideCharToEdenPathComponent(winPath.c_str()), edenPath);
}

TEST(StringConvTest, testWideCharToEdenPathComponentPiece) {
  std::wstring winPath = L"FileName.txt";
  PathComponentPiece edenPath = "FileName.txt"_pc;
  EXPECT_EQ(wideCharToEdenPathComponent(winPath).piece(), edenPath);
}

TEST(StringConvTest, testWideCharToEdenPathComponentPieceCstr) {
  std::wstring winPath = L"FileName.txt";
  PathComponentPiece edenPath = "FileName.txt"_pc;
  EXPECT_EQ(wideCharToEdenPathComponent(winPath.c_str()).piece(), edenPath);
}

TEST(StringConvTest, testWideCharToEdenRelativePath) {
  std::wstring winPath = L"mixed\\winPath/PATH1\\path/File.txt";
  RelativePath edenPath{"mixed/winPath/PATH1/path/File.txt"_relpath};
  EXPECT_EQ(wideCharToEdenRelativePath(winPath), edenPath);
}

TEST(StringConvTest, testWideCharToEdenRelativePathCstr) {
  std::wstring winPath = L"mixed\\winPath/PATH1\\path/File.txt";
  RelativePath edenPath{"mixed/winPath/PATH1/path/File.txt"_relpath};
  EXPECT_EQ(wideCharToEdenRelativePath(winPath.c_str()), edenPath);
}
TEST(StringConvTest, testWideCharToEdenRelativePathPiece) {
  std::wstring winPath = L"mixed/winPath\\PATH1/path\\File.txt";
  auto edenPath = "mixed/winPath/PATH1/path/File.txt"_relpath;
  EXPECT_EQ(wideCharToEdenRelativePath(winPath).piece(), edenPath);
}

TEST(StringConvTest, testWideCharToEdenRelativePathPieceCstr) {
  std::wstring winPath = L"mixed/winPath\\PATH1/path\\File.txt";
  auto edenPath = "mixed/winPath/PATH1/path/File.txt"_relpath;
  EXPECT_EQ(wideCharToEdenRelativePath(winPath.c_str()).piece(), edenPath);
}
