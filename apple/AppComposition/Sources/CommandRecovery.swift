import AlondraCommands
import Foundation

/// One identity owns one polling loop and a durable replay of verified terminal outcomes.
/// The status closure must only reconcile an existing request; it may never dispatch a POST.
public actor CommandRecovery {
    private let gate: CommandGate
    private let scope: CommandScope
    private let file: URL
    private let status: @Sendable (UUID) async throws -> Int?
    private var resolutions: [UUID: CommandResolution] = [:]
    private var loaded = false
    private var listeners: [UUID: AsyncStream<CommandResolution>.Continuation] = [:]
    private var polling: Task<Void, Never>?
    private var active = true
    private var recovering = false

    public init(gate: CommandGate, scope: CommandScope, file: URL,
                status: @escaping @Sendable (UUID) async throws -> Int?) {
        self.gate = gate; self.scope = scope; self.file = file; self.status = status
    }

    public func subscribe() -> AsyncStream<CommandResolution> {
        guard active else { return AsyncStream { $0.finish() } }
        let id = UUID()
        let stream = AsyncStream<CommandResolution> { continuation in
            listeners[id] = continuation
            continuation.onTermination = { _ in Task { await self.remove(id) } }
        }
        if polling == nil {
            polling = Task {
                while !Task.isCancelled {
                    await self.recoverOnce()
                    do { try await Task.sleep(for: .seconds(5)) } catch { break }
                }
            }
        }
        return stream
    }

    public func stop() {
        active = false
        polling?.cancel(); polling = nil
        for listener in listeners.values { listener.finish() }
        listeners = [:]
    }

    private func remove(_ id: UUID) {
        listeners[id] = nil
        if listeners.isEmpty { polling?.cancel(); polling = nil }
    }

    /// A failed journal write or status lookup leaves the command blocked.
    public func recoverOnce() async {
        guard active, !Task.isCancelled, !recovering else { return }
        recovering = true
        defer { recovering = false }
        do {
            if !loaded {
                if FileManager.default.fileExists(atPath: file.path) {
                    let records = try JSONDecoder().decode([CommandResolution].self, from: Data(contentsOf: file))
                    guard records.allSatisfy({ $0.command.scope.sameIdentity(as: scope) && (200..<500).contains($0.status) }) else { return }
                    resolutions = Dictionary(records.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
                }
                loaded = true
            }
            let unresolved = try await gate.restore()
            for command in unresolved {
                guard active, !Task.isCancelled else { return }
                if case .replyToReport = command.kind { continue } // Two-step feedback owns its own recovery.
                if resolutions[command.id] == nil {
                    do {
                        guard let code = try await status(command.id), (200..<500).contains(code) else { continue }
                        let resolution = CommandResolution(command: command, status: code)
                        var next = resolutions; next[command.id] = resolution
                        try persist(next)
                        resolutions = next
                    } catch { continue }
                }
            }
            for resolution in resolutions.values {
                guard active, !Task.isCancelled else { return }
                guard await gate.confirmReconciled(resolution.id, in: scope) else { continue }
                guard active, !Task.isCancelled else { return }
                for listener in listeners.values { listener.yield(resolution) }
            }
        } catch {
            // Retain uncertainty. A later iteration can recover after transient I/O/auth failure.
        }
    }

    private func persist(_ records: [UUID: CommandResolution]) throws {
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        let bytes = try JSONEncoder().encode(Array(records.values))
        try bytes.write(to: file, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
    }
}
