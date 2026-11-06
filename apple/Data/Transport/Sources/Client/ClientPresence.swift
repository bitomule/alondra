import Foundation

/// Input renews presence independently for each open Mac; idle windows send nothing.
public actor ClientPresence {
    private var lastSent: [String: Duration] = [:]
    private let now: @Sendable () -> Duration

    public init(now: @escaping @Sendable () -> Duration = {
        .seconds(ProcessInfo.processInfo.systemUptime)
    }) {
        self.now = now
    }

    public func activity(active: Bool, clients: [String: AlondraAPIClient]) async {
        guard active else { return }
        let time = now()
        lastSent = lastSent.filter { clients[$0.key] != nil }
        let due = clients.filter { id, _ in
            lastSent[id].map { time - $0 >= .seconds(30) } ?? true
        }
        for id in due.keys { lastSent[id] = time }
        await withTaskGroup(of: Void.self) { group in
            for client in due.values {
                group.addTask { try? await client.reportMacPresence() }
            }
        }
    }
}

public extension AlondraAPIClient {
    func reportMacPresence() async throws {
        let body = try JSONEncoder().encode(["client": "mac"])
        _ = try await send(Endpoint<Data>.data(.post, path: ["activity"], body: body,
                                             contentType: "application/json"), operationID: UUID().uuidString)
    }
}
