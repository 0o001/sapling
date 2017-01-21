/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include <gtest/gtest.h>

#include "eden/fs/model/git/GlobMatcher.h"

using namespace facebook::eden;
using folly::ByteRange;
using folly::StringPiece;

// Unfortunately we can't just say EXPECT_EQ(expected, match(...)) below,
// due to a gcc / gtest bug: https://github.com/google/googletest/issues/322
// We have to explicitly break this out in to separate EXPECT_TRUE /
// EXPECT_FALSE checks.
#define EXPECT_MATCH_IMPL(text, glob, expected)            \
  do {                                                     \
    auto matcher = GlobMatcher::create(glob);              \
    if (!matcher.hasValue()) {                             \
      ADD_FAILURE() << "failed to compile glob \"" << glob \
                    << "\": " << matcher.error();          \
    } else if (expected) {                                 \
      EXPECT_TRUE(matcher.value().match(text));            \
    } else {                                               \
      EXPECT_FALSE(matcher.value().match(text));           \
    }                                                      \
  } while (0)
#define EXPECT_MATCH(text, glob) EXPECT_MATCH_IMPL(text, glob, true)
#define EXPECT_NOMATCH(text, glob) EXPECT_MATCH_IMPL(text, glob, false)
#define EXPECT_BADGLOB(glob) EXPECT_TRUE(GlobMatcher::create(glob).hasError())

TEST(Glob, testsFromGit) {
  // Patterns taken from git's test cases,
  // to ensure we are compatible with it's behavior.

  // Basic wildmatch features
  EXPECT_MATCH("foo", "foo");
  EXPECT_NOMATCH("foo", "bar");
  EXPECT_MATCH("", "");
  EXPECT_MATCH("foo", "???");
  EXPECT_NOMATCH("foo", "??");
  EXPECT_MATCH("foo", "*");
  EXPECT_MATCH("foo", "f*");
  EXPECT_NOMATCH("foo", "*f");
  EXPECT_MATCH("foo", "*foo*");
  EXPECT_MATCH("foobar", "*ob*a*r*");
  EXPECT_MATCH("aaaaaaabababab", "*ab");
  EXPECT_MATCH("foo*", "foo\\*");
  EXPECT_NOMATCH("foobar", "foo\\*bar");
  EXPECT_MATCH("f\\oo", "f\\\\oo");
  EXPECT_MATCH("ball", "*[al]?");
  EXPECT_NOMATCH("ten", "[ten]");
  EXPECT_BADGLOB("**[!te]");
  EXPECT_BADGLOB("**[!ten]");
  EXPECT_MATCH("ten", "t[a-g]n");
  EXPECT_NOMATCH("ten", "t[!a-g]n");
  EXPECT_MATCH("ton", "t[!a-g]n");
  EXPECT_MATCH("ton", "t[^a-g]n");
  EXPECT_MATCH("a]b", "a[]]b");
  EXPECT_MATCH("a-b", "a[]-]b");
  EXPECT_MATCH("a]b", "a[]-]b");
  EXPECT_NOMATCH("aab", "a[]-]b");
  EXPECT_MATCH("aab", "a[]a-]b");
  EXPECT_MATCH("]", "]");

  // Extended slash-matching features
  EXPECT_NOMATCH("foo/baz/bar", "foo*bar");
  EXPECT_BADGLOB("foo**bar");
  EXPECT_BADGLOB("foo**bar");
  EXPECT_MATCH("foo/baz/bar", "foo/**/bar");
  EXPECT_MATCH("foo/baz/bar", "foo/**/**/bar");
  EXPECT_MATCH("foo/b/a/z/bar", "foo/**/bar");
  EXPECT_MATCH("foo/b/a/z/bar", "foo/**/**/bar");
  EXPECT_MATCH("foo/bar", "foo/**/bar");
  EXPECT_MATCH("foo/bar", "foo/**/**/bar");
  EXPECT_NOMATCH("foo/bar", "foo?bar");
  EXPECT_NOMATCH("foo/bar", "foo[/]bar");
  EXPECT_NOMATCH("foo/bar", "f[^eiu][^eiu][^eiu][^eiu][^eiu]r");
  EXPECT_MATCH("foo-bar", "f[^eiu][^eiu][^eiu][^eiu][^eiu]r");
  EXPECT_MATCH("foo", "**/foo");
  EXPECT_MATCH("XXX/foo", "**/foo");
  EXPECT_MATCH("bar/baz/foo", "**/foo");
  EXPECT_NOMATCH("bar/baz/foo", "*/foo");
  EXPECT_NOMATCH("foo/bar/baz", "**/bar*");
  EXPECT_MATCH("deep/foo/bar/baz", "**/bar/*");
  EXPECT_NOMATCH("deep/foo/bar/baz/", "**/bar/*");
  EXPECT_MATCH("deep/foo/bar/baz/", "**/bar/**");
  EXPECT_NOMATCH("deep/foo/bar", "**/bar/*");
  EXPECT_MATCH("deep/foo/bar/", "**/bar/**");
  EXPECT_BADGLOB("**/bar**");
  EXPECT_MATCH("foo/bar/baz/x", "*/bar/**");
  EXPECT_NOMATCH("deep/foo/bar/baz/x", "*/bar/**");
  EXPECT_MATCH("deep/foo/bar/baz/x", "**/bar/*/*");

  // Various additional tests
  EXPECT_NOMATCH("acrt", "a[c-c]st");
  EXPECT_MATCH("acrt", "a[c-c]rt");
  EXPECT_NOMATCH("]", "[!]-]");
  EXPECT_MATCH("a", "[!]-]");
  EXPECT_BADGLOB("\\");
  EXPECT_BADGLOB("*/\\");
  EXPECT_MATCH("XXX/\\", "*/\\\\");
  EXPECT_MATCH("foo", "foo");
  EXPECT_MATCH("@foo", "@foo");
  EXPECT_NOMATCH("foo", "@foo");
  EXPECT_MATCH("[ab]", "\\[ab]");
  EXPECT_MATCH("[ab]", "[[]ab]");
  EXPECT_MATCH("[ab]", "[[:]ab]");
  EXPECT_BADGLOB("[[::]ab]");
  EXPECT_MATCH("[ab]", "[[:digit]ab]");
  EXPECT_MATCH("[ab]", "[\\[:]ab]");
  EXPECT_MATCH("?a?b", "\\??\\?b");
  EXPECT_MATCH("abc", "\\a\\b\\c");
  EXPECT_NOMATCH("foo", "");
  EXPECT_MATCH("foo/bar/baz/to", "**/t[o]");

  // Character class tests
  EXPECT_MATCH("a1B", "[[:alpha:]][[:digit:]][[:upper:]]");
  EXPECT_NOMATCH("a", "[[:digit:][:upper:][:space:]]");
  EXPECT_MATCH("A", "[[:digit:][:upper:][:space:]]");
  EXPECT_MATCH("1", "[[:digit:][:upper:][:space:]]");
  EXPECT_BADGLOB("[[:digit:][:upper:][:spaci:]]");
  EXPECT_MATCH(" ", "[[:digit:][:upper:][:space:]]");
  EXPECT_NOMATCH(".", "[[:digit:][:upper:][:space:]]");
  EXPECT_MATCH(".", "[[:digit:][:punct:][:space:]]");
  EXPECT_MATCH("5", "[[:xdigit:]]");
  EXPECT_MATCH("f", "[[:xdigit:]]");
  EXPECT_MATCH("D", "[[:xdigit:]]");
  EXPECT_MATCH(
      "_",
      "[[:alnum:][:alpha:][:blank:][:cntrl:][:digit:][:graph:]"
      "[:lower:][:print:][:punct:][:space:][:upper:][:xdigit:]]");
  EXPECT_MATCH(
      "_",
      "[[:alnum:][:alpha:][:blank:][:cntrl:][:digit:][:graph:]"
      "[:lower:][:print:][:punct:][:space:][:upper:][:xdigit:]]");
  EXPECT_MATCH(
      ".",
      "[^[:alnum:][:alpha:][:blank:][:cntrl:][:digit:][:lower:]"
      "[:space:][:upper:][:xdigit:]]");
  EXPECT_MATCH("5", "[a-c[:digit:]x-z]");
  EXPECT_MATCH("b", "[a-c[:digit:]x-z]");
  EXPECT_MATCH("y", "[a-c[:digit:]x-z]");
  EXPECT_NOMATCH("q", "[a-c[:digit:]x-z]");

  // Additional tests, including some malformed wildmats
  EXPECT_MATCH("]", "[\\\\-^]");
  EXPECT_NOMATCH("[", "[\\\\-^]");
  EXPECT_MATCH("-", "[\\-_]");
  EXPECT_MATCH("]", "[\\]]");
  EXPECT_NOMATCH("\\]", "[\\]]");
  EXPECT_NOMATCH("\\", "[\\]]");
  EXPECT_BADGLOB("ab[");
  EXPECT_BADGLOB("[!");
  EXPECT_BADGLOB("[-");
  EXPECT_MATCH("-", "[-]");
  EXPECT_BADGLOB("[a-");
  EXPECT_BADGLOB("[!a-");
  EXPECT_MATCH("-", "[--A]");
  EXPECT_MATCH("5", "[--A]");
  EXPECT_MATCH(" ", "[ --]");
  EXPECT_MATCH("$", "[ --]");
  EXPECT_MATCH("-", "[ --]");
  EXPECT_NOMATCH("0", "[ --]");
  EXPECT_MATCH("-", "[---]");
  EXPECT_MATCH("-", "[------]");
  EXPECT_NOMATCH("j", "[a-e-n]");
  EXPECT_MATCH("-", "[a-e-n]");
  EXPECT_MATCH("a", "[!------]");
  EXPECT_NOMATCH("[", "[]-a]");
  EXPECT_MATCH("^", "[]-a]");
  EXPECT_NOMATCH("^", "[!]-a]");
  EXPECT_MATCH("[", "[!]-a]");
  EXPECT_MATCH("^", "[a^bc]");
  EXPECT_MATCH("-b]", "[a-]b]");
  EXPECT_BADGLOB("[\\]");
  EXPECT_MATCH("\\", "[\\\\]");
  EXPECT_NOMATCH("\\", "[!\\\\]");
  EXPECT_MATCH("G", "[A-\\\\]");
  EXPECT_NOMATCH("aaabbb", "b*a");
  EXPECT_NOMATCH("aabcaa", "*ba*");
  EXPECT_MATCH(",", "[,]");
  EXPECT_MATCH(",", "[\\\\,]");
  EXPECT_MATCH("\\", "[\\\\,]");
  EXPECT_MATCH("-", "[,-.]");
  EXPECT_NOMATCH("+", "[,-.]");
  EXPECT_NOMATCH("-.]", "[,-.]");
  EXPECT_MATCH("2", "[\\1-\\3]");
  EXPECT_MATCH("3", "[\\1-\\3]");
  EXPECT_NOMATCH("4", "[\\1-\\3]");
  EXPECT_MATCH("\\", "[[-\\]]");
  EXPECT_MATCH("[", "[[-\\]]");
  EXPECT_MATCH("]", "[[-\\]]");
  EXPECT_NOMATCH("-", "[[-\\]]");

  // Test recursion
  EXPECT_MATCH(
      "-adobe-courier-bold-o-normal--12-120-75-75-m-70-iso8859-1",
      "-*-*-*-*-*-*-12-*-*-*-m-*-*-*");
  EXPECT_NOMATCH(
      "-adobe-courier-bold-o-normal--12-120-75-75-X-70-iso8859-1",
      "-*-*-*-*-*-*-12-*-*-*-m-*-*-*");
  EXPECT_NOMATCH(
      "-adobe-courier-bold-o-normal--12-120-75-75-/-70-iso8859-1",
      "-*-*-*-*-*-*-12-*-*-*-m-*-*-*");
  EXPECT_MATCH(
      "XXX/adobe/courier/bold/o/normal//12/120/75/75/m/70/iso8859/1",
      "XXX/*/*/*/*/*/*/12/*/*/*/m/*/*/*");
  EXPECT_NOMATCH(
      "XXX/adobe/courier/bold/o/normal//12/120/75/75/X/70/iso8859/1",
      "XXX/*/*/*/*/*/*/12/*/*/*/m/*/*/*");
  EXPECT_MATCH(
      "abcd/abcdefg/abcdefghijk/abcdefghijklmnop.txt", "**/*a*b*g*n*t");
  EXPECT_NOMATCH(
      "abcd/abcdefg/abcdefghijk/abcdefghijklmnop.txtz", "**/*a*b*g*n*t");
  EXPECT_NOMATCH("foo", "*/*/*");
  EXPECT_NOMATCH("foo/bar", "*/*/*");
  EXPECT_MATCH("foo/bba/arr", "*/*/*");
  EXPECT_NOMATCH("foo/bb/aa/rr", "*/*/*");
  EXPECT_MATCH("foo/bb/aa/rr", "**/**/**");
  EXPECT_MATCH("abcXdefXghi", "*X*i");
  EXPECT_NOMATCH("ab/cXd/efXg/hi", "*X*i");
  EXPECT_MATCH("ab/cXd/efXg/hi", "*/*X*/*/*i");
  EXPECT_MATCH("ab/cXd/efXg/hi", "**/*X*/**/*i");

  // Case-sensitivity features
  EXPECT_NOMATCH("a", "[A-Z]");
  EXPECT_MATCH("A", "[A-Z]");
  EXPECT_NOMATCH("A", "[a-z]");
  EXPECT_MATCH("a", "[a-z]");
  EXPECT_NOMATCH("a", "[[:upper:]]");
  EXPECT_MATCH("A", "[[:upper:]]");
  EXPECT_NOMATCH("A", "[[:lower:]]");
  EXPECT_MATCH("a", "[[:lower:]]");
  EXPECT_NOMATCH("A", "[B-Za]");
  EXPECT_MATCH("a", "[B-Za]");
  EXPECT_NOMATCH("A", "[B-a]");
  EXPECT_MATCH("a", "[B-a]");
  EXPECT_NOMATCH("z", "[Z-y]");
  EXPECT_MATCH("Z", "[Z-y]");
}

TEST(Glob, testOther) {
  // Test parsing "**" by itself
  EXPECT_BADGLOB("**");

  // Test a range expression using non-ASCII byte values
  EXPECT_MATCH("foo\xaatest", "foo[\xa0-\xaf]test");
  EXPECT_NOMATCH("foo\xaatest", "foo[\xb0-\xbf]test");
  EXPECT_NOMATCH("foo\x9atest", "foo[\xa0-\xaf]test");
}

void testCharClass(StringPiece name, int (*libcFn)(int)) {
  auto matcher = GlobMatcher::create("[[:" + name.str() + ":]]").value();

  uint8_t ch = 0;
  StringPiece text{ByteRange(&ch, 1)};
  while (true) {
    // '/' is special, and never matches.
    // Anything outside of the ASCII range should also always return false.
    // (The libc functions may behave differently for these characters
    // depending on the current locale settings.)
    if (ch == '/' || ch >= 0x80) {
      EXPECT_FALSE(matcher.match(text)) << "character class \"" << name
                                        << "\", character " << (int)ch;
    } else {
      EXPECT_EQ((bool)libcFn(ch), matcher.match(text))
          << "character class \"" << name << "\", character " << (int)ch;
    }
    if (ch == 0xff) {
      break;
    }
    ++ch;
  }
}

TEST(Glob, testCharClasses) {
  // Make sure all of our character classes agree with the
  // builtin libc functions.
  testCharClass("alnum", isalnum);
  testCharClass("alpha", isalpha);
  testCharClass("blank", isblank);
  testCharClass("cntrl", iscntrl);
  testCharClass("digit", isdigit);
  testCharClass("graph", isgraph);
  testCharClass("lower", islower);
  testCharClass("print", isprint);
  testCharClass("punct", ispunct);
  testCharClass("space", isspace);
  testCharClass("upper", isupper);
  testCharClass("xdigit", isxdigit);
}
