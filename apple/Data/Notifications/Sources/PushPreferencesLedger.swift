import Darwin
import Foundation

@_silgen_name("flock")
private func pushPreferencesLock(_ descriptor: Int32, _ operation: Int32) -> Int32

/// Registration state survives app/actor recreation. The executor accepts only a
/// newer revision, or the identical snapshot at the same revision.
public actor PushPreferencesLedger {
    private struct Record: Codable {
        let binding: String
        let quietStart: String?
        let quietEnd: String?
        let revision: UInt64
    }
    private let file: URL
    public init(file: URL) { self.file = file }

    public func reserve(identity: String, binding: String, quietStart: String?, quietEnd: String?,
                        newerThan minimum: UInt64 = 0) throws -> UInt64 {
        let directory = file.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        let lock = Darwin.open(file.appendingPathExtension("lock").path, O_RDWR | O_CREAT, 0o600)
        guard lock >= 0 else { throw posixError() }
        defer { _ = Darwin.close(lock) }
        guard pushPreferencesLock(lock, LOCK_EX) == 0 else { throw posixError() }
        defer { _ = pushPreferencesLock(lock, LOCK_UN) }
        var records: [String: Record]
        do { records = try JSONDecoder().decode([String: Record].self, from: Data(contentsOf: file)) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { records = [:] }
        let old = records[identity]
        if let old, old.binding == binding, old.quietStart == quietStart,
           old.quietEnd == quietEnd, old.revision > minimum { return old.revision }
        let previous = max(old?.revision ?? 0, minimum)
        guard previous < 9_007_199_254_740_991 else { throw LedgerError.exhausted }
        let revision = previous + 1
        records[identity] = Record(binding: binding, quietStart: quietStart, quietEnd: quietEnd, revision: revision)
        let data = try JSONEncoder().encode(records)
        let temporary = directory.appendingPathComponent(".push-preferences-\(UUID().uuidString)")
        let descriptor = Darwin.open(temporary.path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
        guard descriptor >= 0 else { throw posixError() }
        defer { _ = Darwin.close(descriptor); _ = Darwin.unlink(temporary.path) }
        try data.withUnsafeBytes { bytes in
            var offset = 0
            while offset < bytes.count {
                let count = Darwin.write(descriptor, bytes.baseAddress!.advanced(by: offset), bytes.count - offset)
                if count < 0 && errno == EINTR { continue }
                guard count > 0 else { throw posixError() }
                offset += count
            }
        }
        guard Darwin.fsync(descriptor) == 0, Darwin.rename(temporary.path, file.path) == 0 else { throw posixError() }
        let parent = Darwin.open(directory.path, O_RDONLY)
        guard parent >= 0 else { throw posixError() }
        defer { _ = Darwin.close(parent) }
        guard Darwin.fsync(parent) == 0 else { throw posixError() }
        return revision
    }
    private func posixError() -> POSIXError { POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
    private enum LedgerError: Error { case exhausted }
}
