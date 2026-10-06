import Darwin
import Foundation

@_silgen_name("flock")
private func pushUnregistrationLock(_ descriptor: Int32, _ operation: Int32) -> Int32

/// Bindings the relay must forget, kept on disk until it confirms: sign-out cannot wait
/// for the network, and a killed process must not leave a Mac pushing to a dead binding.
public struct PushUnregistrationLedger: Sendable {
    public struct Entry: Codable, Hashable, Sendable {
        public let relayURL: URL
        public let pushBinding: String

        public init(relayURL: URL, pushBinding: String) {
            self.relayURL = relayURL
            self.pushBinding = pushBinding
        }

        enum CodingKeys: String, CodingKey { case relayURL = "relay_url", pushBinding = "push_binding" }
    }

    private let file: URL
    public init(file: URL) { self.file = file }

    public func enqueue(_ entries: [Entry]) throws {
        try mutate { pending in
            for entry in entries where !pending.contains(entry) { pending.append(entry) }
        }
    }

    public func pending() throws -> [Entry] { try mutate { _ in } }

    public func remove(_ entries: [Entry]) throws {
        let removed = Set(entries)
        try mutate { pending in pending.removeAll { removed.contains($0) } }
    }

    @discardableResult
    private func mutate(_ change: (inout [Entry]) -> Void) throws -> [Entry] {
        let directory = file.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        let lock = Darwin.open(file.appendingPathExtension("lock").path, O_RDWR | O_CREAT, 0o600)
        guard lock >= 0 else { throw posixError() }
        defer { _ = Darwin.close(lock) }
        guard pushUnregistrationLock(lock, LOCK_EX) == 0 else { throw posixError() }
        defer { _ = pushUnregistrationLock(lock, LOCK_UN) }
        var entries: [Entry]
        do { entries = try JSONDecoder().decode([Entry].self, from: Data(contentsOf: file)) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { entries = [] }
        catch is DecodingError { entries = [] }
        let original = entries
        change(&entries)
        guard entries != original else { return entries }
        try JSONEncoder().encode(entries).write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        return entries
    }

    private func posixError() -> POSIXError { POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
}
