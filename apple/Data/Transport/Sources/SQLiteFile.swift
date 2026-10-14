import Darwin
import Foundation
import SQLite3

/// The one way the app opens a SQLite file. SQLite's NOFOLLOW refuses a symbolic link anywhere in
/// the path, and every iOS app container is reached through one (/var is /private/var), so the
/// folder is resolved first and only the database file itself must not be a link.
public enum SQLiteFile {
    public struct Unresolvable: Error, Equatable, Sendable {}

    public static func resolvedFolder(_ folder: URL) throws -> URL {
        guard let resolved = realpath(folder.path, nil) else { throw Unresolvable() }
        defer { free(resolved) }
        return URL(fileURLWithPath: String(cString: resolved), isDirectory: true)
    }

    public static func path(_ name: String, in folder: URL) throws -> String {
        try resolvedFolder(folder).appendingPathComponent(name).path
    }

    /// Opens `name` inside `folder` with NOFOLLOW added to `flags`; nil when SQLite refuses it.
    public static func open(_ name: String, in folder: URL, flags: Int32) throws -> OpaquePointer? {
        try open(path: path(name, in: folder), flags: flags)
    }

    /// For a path already built by `path(_:in:)`.
    public static func open(path: String, flags: Int32) -> OpaquePointer? {
        var pointer: OpaquePointer?
        guard sqlite3_open_v2(path, &pointer, flags | SQLITE_OPEN_NOFOLLOW, nil) == SQLITE_OK, let database = pointer else {
            if let pointer { sqlite3_close(pointer) }
            return nil
        }
        return database
    }
}
