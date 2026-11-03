import AlondraContract
import CryptoKit
import Foundation
import Observation
import SwiftUI

/// A card's attachments as files on this device: each one is downloaded once from the Mac, checked
/// against its digest, and kept by that digest in a folder that never grows past `limit`, oldest out.
@MainActor @Observable
public final class CardAttachmentFiles {
    public typealias Download = @Sendable (Attachment, URL, @escaping @Sendable (UInt64, UInt64) -> Void) async throws -> Void

    public enum Phase: Equatable, Sendable {
        case idle
        case loading(fraction: Double?)
        case ready(URL)
        case failed(Failure)
    }

    public enum Failure: Equatable, Sendable {
        /// The Mac no longer holds this file as the card names it: trying again will not help.
        case gone
        /// The Mac did not answer: trying again may.
        case unreachable
    }

    @ObservationIgnored private let directory: URL
    @ObservationIgnored private let limit: Int64
    @ObservationIgnored private let download: Download
    @ObservationIgnored private let isGone: @Sendable (Error) -> Bool
    @ObservationIgnored private var tasks: [String: (token: UUID, task: Task<Void, Never>)] = [:]
    private var phases: [String: Phase] = [:]

    public init(directory: URL, limit: Int64 = 512 * 1024 * 1024, download: @escaping Download,
                isGone: @escaping @Sendable (Error) -> Bool) {
        self.directory = directory
        self.limit = limit
        self.download = download
        self.isGone = isGone
    }

    /// `Caches/CardAttachments` of this app.
    public static var defaultDirectory: URL {
        URL.cachesDirectory.appendingPathComponent("CardAttachments", isDirectory: true)
    }

    public func phase(_ attachment: Attachment) -> Phase {
        let key = Self.key(attachment)
        if let phase = phases[key] { return phase }
        let file = location(attachment)
        return FileManager.default.fileExists(atPath: file.path) ? .ready(file) : .idle
    }

    /// Starts the download unless the file is already here or on its way; a failed one starts again.
    public func load(_ attachment: Attachment) {
        let key = Self.key(attachment)
        switch phase(attachment) {
        case .loading: return
        case .ready(let file):
            phases[key] = .ready(file)
            try? FileManager.default.setAttributes([.modificationDate: Date.now], ofItemAtPath: file.path)
            return
        case .idle, .failed: break
        }
        phases[key] = .loading(fraction: nil)
        let destination = location(attachment)
        let directory = directory, download = download, isGone = isGone, limit = limit
        let token = UUID()
        let task = Task {
            let outcome: Phase
            do {
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                let partial = directory.appendingPathComponent(".partial-\(UUID().uuidString)")
                defer { try? FileManager.default.removeItem(at: partial) }
                try await download(attachment, partial) { done, total in
                    let fraction = total == 0 ? nil : Double(done) / Double(total)
                    Task { @MainActor in self.advance(key, token: token, fraction: fraction) }
                }
                try Task.checkCancellation()
                if FileManager.default.fileExists(atPath: destination.path) {
                    try FileManager.default.removeItem(at: destination)
                }
                try FileManager.default.moveItem(at: partial, to: destination)
                Self.trim(directory, to: limit, keeping: destination)
                outcome = .ready(destination)
            } catch is CancellationError {
                outcome = .idle
            } catch {
                outcome = .failed(isGone(error) ? .gone : .unreachable)
            }
            finish(key, token: token, outcome)
        }
        tasks[key] = (token, task)
    }

    public func cancel(_ attachment: Attachment) {
        let key = Self.key(attachment)
        tasks.removeValue(forKey: key)?.task.cancel()
        if case .loading = phases[key] { phases[key] = .idle }
    }

    /// Waits for the download of `attachment` that is under way, if any.
    func settled(_ attachment: Attachment) async {
        await tasks[Self.key(attachment)]?.task.value
    }

    private func advance(_ key: String, token: UUID, fraction: Double?) {
        guard tasks[key]?.token == token, case .loading = phases[key] else { return }
        phases[key] = .loading(fraction: fraction)
    }

    private func finish(_ key: String, token: UUID, _ phase: Phase) {
        guard tasks[key]?.token == token else { return }
        tasks[key] = nil
        phases[key] = phase
    }

    private func location(_ attachment: Attachment) -> URL {
        let name = Self.key(attachment)
        let ext = (attachment.name as NSString).pathExtension.lowercased()
        let safe = ext.count <= 8 && !ext.isEmpty && ext.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber) }
        return directory.appendingPathComponent(safe ? "\(name).\(ext)" : name)
    }

    static func key(_ attachment: Attachment) -> String {
        if let digest = attachment.digest, digest.utf8.count == 64, digest.allSatisfy(\.isHexDigit) { return digest.lowercased() }
        return SHA256.hash(data: Data(attachment.fetch.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    nonisolated static func trim(_ directory: URL, to limit: Int64, keeping kept: URL) {
        let keys: [URLResourceKey] = [.fileSizeKey, .contentModificationDateKey]
        guard let files = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: keys) else { return }
        let entries = files.filter { !$0.lastPathComponent.hasPrefix(".") }.compactMap { file -> (URL, Int64, Date)? in
            guard let values = try? file.resourceValues(forKeys: Set(keys)) else { return nil }
            return (file, Int64(values.fileSize ?? 0), values.contentModificationDate ?? .distantPast)
        }
        var total = entries.reduce(0) { $0 + $1.1 }
        for (file, size, _) in entries.sorted(by: { $0.2 < $1.2 }) where total > limit && file.standardizedFileURL != kept.standardizedFileURL {
            try? FileManager.default.removeItem(at: file)
            total -= size
        }
    }
}

public extension EnvironmentValues {
    @Entry var cardAttachmentFiles: CardAttachmentFiles?
}
