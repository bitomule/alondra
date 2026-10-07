import Foundation

/// Routes only the daemon on this Mac answers, outside the device contract: notices, devices
/// and pending pairing requests. Never sealed into an envelope, never sent to another Mac.
public enum LoopbackLocalRoute: Sendable, Equatable {
    case notices(lang: String)
    case devices
    case pendingDevices
    case approvePending(String)
    case denyPending(String)
    case revokeDevice(String)

    var method: String {
        switch self {
        case .notices, .devices, .pendingDevices: "GET"
        case .approvePending, .denyPending: "POST"
        case .revokeDevice: "DELETE"
        }
    }

    var target: String? {
        switch self {
        case .notices(let lang): ["en", "es"].contains(lang) ? "/notices?lang=\(lang)" : nil
        case .devices: "/devices"
        case .pendingDevices: "/devices/pending"
        case .approvePending(let id): Self.valid(id) ? "/devices/pending/\(id)/approve" : nil
        case .denyPending(let id): Self.valid(id) ? "/devices/pending/\(id)/deny" : nil
        case .revokeDevice(let id): Self.valid(id) ? "/devices/\(id)" : nil
        }
    }

    var body: Data? { method == "POST" ? Data("{}".utf8) : nil }

    private static func valid(_ id: String) -> Bool {
        !id.isEmpty && id.utf8.count <= 128 && id.utf8.allSatisfy {
            (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95
        }
    }
}

extension LoopbackSession {
    public func local(_ route: LoopbackLocalRoute) async throws -> Data {
        guard let target = route.target else { throw OperationError.invalidEndpoint }
        switch try await exchange(method: route.method, target: target, body: route.body, operationID: nil) {
        case .unknown: throw OperationError.outcomeUnknown
        case let .completed(status, body):
            guard (200..<300).contains(status) else { throw APIError.from(status: status, body: body) }
            return body
        }
    }
}
