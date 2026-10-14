import Foundation
import OSLog

/// The latest requests this device made to its Macs, kept as one JSON line each in a file the
/// person can share and written to the system log, to compare connection paths with real use.
public actor ConnectionMeasurements {
    public static let defaultLimit = 2000
    private static let log = Logger(subsystem: "com.bitomule.alondra", category: "connection")

    public nonisolated let file: URL?
    private let limit: Int
    private let writesLog: Bool
    private var entries: [ConnectionMeasurement] = []
    private var linesInFile = 0
    private var loaded = false

    public init(file: URL?, limit: Int = ConnectionMeasurements.defaultLimit, writesLog: Bool = true) {
        self.file = file
        self.limit = limit
        self.writesLog = writesLog
    }

    public func record(_ measurement: ConnectionMeasurement) {
        loadIfNeeded()
        entries.append(measurement)
        if entries.count > limit { entries.removeFirst(entries.count - limit) }
        guard let line = try? Self.encoder.encode(measurement) else { return }
        if writesLog { Self.log.notice("\(String(decoding: line, as: UTF8.self), privacy: .public)") }
        guard let file else { return }
        if linesInFile >= limit + limit / 10 { rewrite(file) } else { append(line, to: file) }
    }

    public func all() -> [ConnectionMeasurement] {
        loadIfNeeded()
        return entries
    }

    public func summary() -> [ConnectionMeasurementSummary] {
        ConnectionMeasurementSummary.groups(all())
    }

    public func clear() {
        entries = []
        loaded = true
        linesInFile = 0
        if let file { try? FileManager.default.removeItem(at: file) }
    }

    private func loadIfNeeded() {
        guard !loaded else { return }
        loaded = true
        guard let file, let data = try? Data(contentsOf: file) else { return }
        let lines = data.split(separator: UInt8(ascii: "\n"))
        linesInFile = lines.count
        entries = lines.suffix(limit).compactMap { try? Self.decoder.decode(ConnectionMeasurement.self, from: Data($0)) }
    }

    private func append(_ line: Data, to file: URL) {
        let manager = FileManager.default
        if !manager.fileExists(atPath: file.path) {
            try? manager.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            guard manager.createFile(atPath: file.path, contents: nil) else { return }
            linesInFile = 0
        }
        guard let handle = try? FileHandle(forWritingTo: file) else { return }
        defer { try? handle.close() }
        guard (try? handle.seekToEnd()) != nil, (try? handle.write(contentsOf: line + Data("\n".utf8))) != nil else { return }
        linesInFile += 1
    }

    private func rewrite(_ file: URL) {
        let lines = entries.compactMap { try? Self.encoder.encode($0) }
        let data = lines.reduce(into: Data()) { $0.append($1); $0.append(UInt8(ascii: "\n")) }
        try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        guard (try? data.write(to: file, options: .atomic)) != nil else { return }
        linesInFile = lines.count
    }

    static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = .sortedKeys
        return encoder
    }()

    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return decoder
    }()
}
