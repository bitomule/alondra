import Foundation

public enum AccountDeviceError: Error, Equatable, Sendable {
    case removedFromWeb
}

/// Account metadata is authenticated with the bearer, independently of a device's enrollment.
public actor RelayAccountDirectory {
    private let base: URL
    private let bearer: @Sendable () async throws -> String
    private let transport: any HTTPTransport
    private var removals: Set<String> = []

    public init(base: URL, bearer: @escaping @Sendable () async throws -> String,
                transport: (any HTTPTransport)? = nil) throws {
        guard base.scheme == "https", base.host != nil, base.user == nil, base.password == nil,
              base.query == nil, base.fragment == nil, base.path.isEmpty || base.path == "/" else {
            throw RelayWireError.invalidEndpoint
        }
        self.base = base; self.bearer = bearer
        self.transport = transport ?? AlondraAPIClient.makeHTTPTransport()
    }

    public func accountName() async throws -> String {
        struct Account: Decodable { let display_name: String }
        struct Reply: Decodable { let account: Account }
        let reply: Reply = try await get("/v1/account")
        let name = reply.account.display_name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name.count <= 256,
              !name.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else {
            throw RelayWireError.invalidResponse
        }
        return name
    }

    public func isRemoved(macID: String, deviceID: String) async throws -> Bool {
        struct Client: Decodable { let device_id: String; let status: String }
        struct Mac: Decodable { let mac_id: String; let status: String; let clients: [Client] }
        struct Reply: Decodable { let macs: [Mac] }
        let key = macID + "/" + deviceID
        if removals.contains(key) { return true }
        let reply: Reply = try await get("/v1/account/devices")
        let removed = reply.macs.contains { mac in
            mac.mac_id == macID && (mac.status == "revoked" || mac.clients.contains {
                $0.device_id == deviceID && $0.status == "revoked"
            })
        }
        if removed { removals.insert(key) }
        return removed
    }

    private func get<Reply: Decodable>(_ path: String) async throws -> Reply {
        var request = URLRequest(url: URL(string: path, relativeTo: base)!.absoluteURL)
        request.timeoutInterval = 15
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let token = try await bearer()
        guard !token.isEmpty, !token.utf8.contains(where: { $0 <= 32 || $0 == 127 }) else {
            throw RelayWireError.invalidField
        }
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let reply = try await transport.send(request)
        try Task.checkCancellation()
        guard reply.status == 200 else {
            throw reply.status == 401 ? RelayWireError.unauthorized : RelayWireError.server(reply.status)
        }
        return try JSONDecoder().decode(Reply.self, from: reply.body)
    }
}
