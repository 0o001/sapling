include "common/fb303/if/fb303.thrift"

namespace cpp2 facebook.eden
namespace java com.facebook.eden.thrift
namespace py facebook.eden

/** Thrift doesn't really do unsigned numbers, but we can sort of fake it.
 * This type is serialized as an integer value that is 64-bits wide and
 * should round-trip with full fidelity for C++ client/server, but for
 * other runtimes will have crazy results if the sign bit is ever set.
 * In practice it is impossible for us to have files that large in eden,
 * and sequence numbers will take an incredibly long time to ever roll
 * over and cause problems.
 * Once t13345978 is done, we can uncomment the cpp.type below.
 */
typedef i64 /* (cpp.type = "std::uint64_t") */ unsigned64

/**
 * A source control hash, as a 20-byte binary value.
 */
typedef binary BinaryHash

exception EdenError {
  1: required string message
  2: optional i32 errorCode
} (message = 'message')


struct MountInfo {
  1: string mountPoint
  2: string edenClientPath
}

union SHA1Result {
  1: BinaryHash sha1
  2: EdenError error
}

// Effectively a `struct timespec`
struct TimeSpec {
  1: i64 seconds
  2: i64 nanoSeconds
}

// Information that we return when querying entries
struct FileInformation {
  1: unsigned64 size        // wish thrift had unsigned numbers
  2: TimeSpec mtime
  3: i32 mode        // mode_t
}

/** Holds information about a file, or an error in retrieving that info.
 * The most likely error will be ENOENT, implying that the file doesn't exist.
 */
union FileInformationOrError {
  1: FileInformation info
  2: EdenError error
}

/** reference a point in time in the journal.
 * This can be used to reason about a point in time in a given mount point.
 * The mountGeneration value is opaque to the client.
 */
struct JournalPosition {
  /** An opaque but unique number within the scope of a given mount point.
   * This is used to determine when sequenceNumber has been invalidated. */
  1: i64 mountGeneration

  /** Monotonically incrementing number
   * Each journalled change causes this number to increment. */
  2: unsigned64 sequenceNumber

  /** Records the snapshot hash at the appropriate point in the journal */
  3: binary snapshotHash
}

/** Holds information about a set of paths that changed between two points.
 * fromPosition, toPosition define the time window.
 * paths holds the list of paths that changed in that window.
 */
struct FileDelta {
  /** The fromPosition passed to getFilesChangedSince */
  1: JournalPosition fromPosition
  /** The current position at the time that getFilesChangedSince was called */
  2: JournalPosition toPosition
  /** The complete list of paths from both the snapshot and the overlay that
   * changed between fromPosition and toPosition */
  3: list<string> paths
}

enum StatusCode {
  CLEAN = 0x0,
  MODIFIED = 0x1,
  ADDED = 0x2,
  REMOVED = 0x3,
  MISSING = 0x4,
  NOT_TRACKED = 0x5,
  IGNORED = 0x6,
}

struct ThriftHgStatus {
  1: map<string, StatusCode> entries
}

/**
 * Note that the error message always contains the path, so it can be displayed
 * to the user verbatim without having to prefix it with the path explicitly.
 */
struct ScmAddRemoveError {
  1: string path
  2: string errorMessage
}

enum ConflictType {
  // We failed to update this particular path due to an error
  ERROR,
  // A locally modified file was deleted in the new Tree
  MODIFIED_REMOVED,
  // An untracked local file exists in the new Tree
  UNTRACKED_ADDED,
  // The file was removed locally, but modified in the new Tree
  REMOVED_MODIFIED,
  // The file was removed locally, and also removed in the new Tree.
  MISSING_REMOVED,
  // A locally modified file was modified in the new Tree
  // This may be contents modifications, or a file type change (directory to
  // file or vice-versa), or permissions changes.
  MODIFIED,
  // A directory was supposed to be removed or replaced with a file,
  // but it contains untracked files preventing us from updating it.
  DIRECTORY_NOT_EMPTY,
}

/**
 * Details about conflicts or errors that occurred during a checkout operation
 */
struct CheckoutConflict {
  1: string path
  2: ConflictType type
  3: string message
}

service EdenService extends fb303.FacebookService {
  list<MountInfo> listMounts() throws (1: EdenError ex)
  void mount(1: MountInfo info) throws (1: EdenError ex)
  void unmount(1: string mountPoint) throws (1: EdenError ex)

  /**
   * Get the current snapshot that is checked out in the given mount point.
   */
  BinaryHash getCurrentSnapshot(1: string mountPoint)
    throws (1: EdenError ex)

  /**
   * Check out the specified snapshot.
   *
   * This updates the contents of the mount point so that they match the
   * contents of the given snapshot.
   *
   * Returns a list of conflicts and errors that occurred when performing the
   * checkout operation.
   *
   * If the force parameter is true, the working directory will be forcibly
   * updated to the contents of the new snapshot, even if there were conflicts.
   * Conflicts will still be reported in the return value, but the files will
   * be updated to their new state.  If the force parameter is false files with
   * conflicts will be left unmodified.  Files that are untracked in both the
   * source and destination snapshots are always left unchanged, even if force
   * is true.
   *
   * On successful return from this function the mount point will point to the
   * new commit, even if some paths had conflicts or errors.  The caller is
   * responsible for taking appropriate action to update these paths as desired
   * after checkOutRevision() returns.
   */
  list<CheckoutConflict> checkOutRevision(
    1: string mountPoint,
    2: string snapshotHash,
    3: bool force)
      throws (1: EdenError ex)

  // Mount-specific APIs.

  /**
   * For each path, returns an EdenError instead of the SHA-1 if any of the
   * following occur:
   * - path is the empty string.
   * - path identifies a non-existent file.
   * - path identifies something that is not an ordinary file (e.g., symlink
   *   or directory).
   */
  list<SHA1Result> getSHA1(1: string mountPoint, 2: list<string> paths)
    throws (1: EdenError ex)

  /**
   * Returns a list of paths relative to the mountPoint.
   */
  list<string> getBindMounts(1: string mountPoint)
    throws (1: EdenError ex)

  /** Returns the sequence position at the time the method is called.
   * Returns the instantaneous value of the journal sequence number.
   */
  JournalPosition getCurrentJournalPosition(1: string mountPoint)
    throws (1: EdenError ex)

  /** Returns the set of files (and dirs) that changed since a prior point.
   * If fromPosition.mountGeneration is mismatched with the current
   * mountGeneration, throws an EdenError with errorCode = ERANGE.
   * This indicates that eden cannot compute the delta for the requested
   * range.  The client will need to recompute a new baseline using
   * other available functions in EdenService.
   */
  FileDelta getFilesChangedSince(
    1: string mountPoint,
    2: JournalPosition fromPosition)
      throws (1: EdenError ex)

  /** Returns a subset of the stat() information for a list of paths.
   * The returned list of information corresponds to the input list of
   * paths; eg; result[0] holds the information for paths[0].
   * We only support returning the instantaneous information about
   * these paths, as we cannot answer with historical information about
   * files in the overlay.
   */
  list<FileInformationOrError> getFileInformation(
    1: string mountPoint,
    2: list<string> paths)
      throws (1: EdenError ex)

  /** Returns a list of files that match the input globs.
   * There are no duplicate values in the result.
   * wildMatchFlags can hold various WildMatchFlags values OR'd together.
   */
  list<string> glob(
    1: string mountPoint,
    2: list<string> globs)
      throws (1: EdenError ex)

  //////// HG COMMANDS ////////

  // TODO(mbolin): `hg status` has a ton of command line flags to support.
  ThriftHgStatus scmGetStatus(
    1: string mountPoint,
    2: bool listIgnored,
  ) throws (1: EdenError ex)

  list<ScmAddRemoveError> scmAdd(
    1: string mountPoint,
    2: list<string> paths, // May be files or directories.
  ) throws (1: EdenError ex)

  list<ScmAddRemoveError> scmRemove(
    1: string mountPoint,
    2: list<string> paths, // May be files or directories.
    3: bool force
  ) throws (1: EdenError ex)

  void scmMarkCommitted(
    1: string mountPoint,
    2: binary commitID,
    3: list<string> pathsToClean,
    4: list<string> pathsToDrop,
  ) throws (1: EdenError ex)
}
