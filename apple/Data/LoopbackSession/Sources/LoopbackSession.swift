import AlondraAPIClient
import Foundation

public enum LoopbackSessionError: Error, Equatable, Sendable {
    case invalidIdentity, invalidResponse, pairingRefused(Int), status(Int)
}

public enum LoopbackOutcome: Sendable, Equatable {
    case completed(status: Int, body: Data)
    case unknown
}

/// Talks to the alondrad on this Mac over signed loopback HTTP. The daemon has no operation-ID
/// deduplication locally, so a mutation whose response is lost is reported as unknown and never
/// sent again.
public actor LoopbackSession: ServiceOperationTransport {
    public static let defaultBase = URL(string: "http://127.0.0.1:47800")!

    public nonisolated let base: URL
    let urlSession: URLSession
    private let identity: LoopbackIdentityStore
    private let deviceName: String
    private let now: @Sendable () -> Date
    private var pairing: Task<String, Error>?
    private var outcomes: [String: LoopbackOutcome] = [:]
    var uploads: [String: LoopbackUpload] = [:]
    var downloads: [String: LoopbackDownload] = [:]
    private var active = true

    public init(base: URL = LoopbackSession.defaultBase, identity: LoopbackIdentityStore, deviceName: String,
                urlSession: URLSession = LoopbackSession.makeURLSession(), now: @escaping @Sendable () -> Date = { .now }) {
        self.base = base
        self.identity = identity
        self.deviceName = deviceName
        self.urlSession = urlSession
        self.now = now
    }

    public static func makeURLSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        configuration.connectionProxyDictionary = [:]
        return URLSession(configuration: configuration)
    }

    /// Unsigned and side-effect free, so it answers before this Mac has paired with the daemon.
    public static func isDaemonRunning(at base: URL = defaultBase, urlSession: URLSession = makeURLSession()) async -> Bool {
        let request = URLRequest(url: base.appending(path: "daemon"), timeoutInterval: 2)
        guard let (_, response) = try? await urlSession.data(for: request) else { return false }
        return (response as? HTTPURLResponse)?.statusCode == 200
    }

    public func deactivate() {
        active = false
        pairing?.cancel()
        pairing = nil
    }

    /// The last known result of a mutation sent by this process. Anything else is unknown:
    /// without a durable outbox, absence is never proof that nothing was sent.
    public func outcome(requestID: String) -> LoopbackOutcome {
        outcomes[requestID] ?? .unknown
    }

    public func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        guard active else { throw CancellationError() }
        let route = request.path.split(separator: "?", maxSplits: 1).first.map(String.init) ?? request.path
        let result: LoopbackOutcome
        switch (request.method, route) {
        case ("POST", let path) where path.hasPrefix("/uploads/"):
            result = try await upload(String(path.dropFirst("/uploads/".count)), request: request)
        case ("GET", "/files/chunk"):
            result = try await fileChunk(request.path)
        default:
            result = try await exchange(method: request.method, target: request.path, body: request.body,
                                        operationID: request.operationID)
        }
        switch result {
        case let .completed(status, body): return .completed(status: status, body: body)
        case .unknown: return .unknown
        }
    }

    /// A 401 comes from the daemon's signature guard, before any handler runs, so pairing again
    /// and repeating the request once cannot apply a mutation twice.
    func exchange(method: String, target: String, body: Data?, operationID: String?,
                  contentType: String = "application/json", timeout: TimeInterval? = nil) async throws -> LoopbackOutcome {
        let isRead = method == "GET"
        let limit = timeout ?? (isRead ? Self.readTimeout(target) : 120)
        var repaired = false
        while true {
            let device = try await pairedDeviceID()
            let request = try await signedRequest(method: method, target: target, body: body, device: device,
                                                  contentType: contentType, timeout: limit)
            let status: Int, data: Data
            do {
                (status, data) = try await send(request)
            } catch let error as URLError where !isRead && !Self.provesNothingWasSent(error) {
                if let operationID { outcomes[operationID] = .unknown }
                return .unknown
            }
            guard active else { throw CancellationError() }
            if status == 401, !repaired {
                repaired = true
                await forget(device: device)
                continue
            }
            let outcome = LoopbackOutcome.completed(status: status, body: data)
            if !isRead, let operationID { outcomes[operationID] = outcome }
            return outcome
        }
    }

    private static func readTimeout(_ target: String) -> TimeInterval {
        guard target.hasPrefix("/changes?"), let components = URLComponents(string: target),
              let wait = components.queryItems?.first(where: { $0.name == "wait" })?.value.flatMap(Int.init),
              (0...50).contains(wait) else { return 30 }
        return TimeInterval(wait + 20)
    }

    private static func provesNothingWasSent(_ error: URLError) -> Bool {
        [.cannotConnectToHost, .cannotFindHost, .dnsLookupFailed, .notConnectedToInternet].contains(error.code)
    }

    /// Errors that prove the daemon never received the request.
    public static func neverSent(_ error: any Error) -> Bool {
        if let error = error as? URLError { return provesNothingWasSent(error) }
        if case .pairingRefused = error as? LoopbackSessionError { return true }
        return false
    }

    private func send(_ request: URLRequest) async throws -> (Int, Data) {
        let (data, response) = try await urlSession.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw LoopbackSessionError.invalidResponse }
        return (http.statusCode, data)
    }

    private func signedRequest(method: String, target: String, body: Data?, device: String,
                               contentType: String, timeout: TimeInterval) async throws -> URLRequest {
        guard target.hasPrefix("/"), let url = URL(string: base.absoluteString + target) else {
            throw ServiceAPIError.invalidEndpoint
        }
        var request = URLRequest(url: url, timeoutInterval: timeout)
        request.httpMethod = method
        request.httpBody = body
        if body != nil { request.setValue(contentType, forHTTPHeaderField: "Content-Type") }
        let timestamp = String(Int64((now().timeIntervalSince1970 * 1000).rounded(.down)))
        let signature = try await identity.signingKey().signature(
            for: LoopbackSignature.message(method: method, target: target, timestamp: timestamp, body: body ?? Data()))
        request.setValue(device, forHTTPHeaderField: "X-Alondra-Device")
        request.setValue(timestamp, forHTTPHeaderField: "X-Alondra-Timestamp")
        request.setValue(signature.base64EncodedString(), forHTTPHeaderField: "X-Alondra-Signature")
        return request
    }

    private func pairedDeviceID() async throws -> String {
        guard active else { throw CancellationError() }
        if let id = await identity.deviceID() { return id }
        if let pairing { return try await pairing.value }
        let task = Task { try await self.pair() }
        pairing = task
        defer { pairing = nil }
        return try await task.value
    }

    private func forget(device: String) async {
        if await identity.deviceID() == device { await identity.forgetDeviceID() }
    }

    /// The pairing routes answer only loopback callers, so the Mac pairs itself without asking
    /// anyone.
    private func pair() async throws -> String {
        struct Invitation: Decodable { let code: String }
        struct Paired: Decodable { let device_id: String }
        var invite = URLRequest(url: base.appending(path: "pairing"), timeoutInterval: 10)
        invite.httpMethod = "POST"
        let (inviteStatus, inviteBody) = try await send(invite)
        guard inviteStatus == 200 else { throw LoopbackSessionError.pairingRefused(inviteStatus) }
        let code = try JSONDecoder().decode(Invitation.self, from: inviteBody).code
        let publicKey = try await identity.signingKey().publicKey.x963Representation.base64EncodedString()
        var pair = URLRequest(url: base.appending(path: "pair"), timeoutInterval: 10)
        pair.httpMethod = "POST"
        pair.setValue("application/json", forHTTPHeaderField: "Content-Type")
        pair.httpBody = try JSONSerialization.data(withJSONObject: [
            "code": code, "device_name": deviceName, "public_key": publicKey,
        ], options: [.sortedKeys])
        let (pairStatus, pairBody) = try await send(pair)
        guard pairStatus == 200 else { throw LoopbackSessionError.pairingRefused(pairStatus) }
        let id = try JSONDecoder().decode(Paired.self, from: pairBody).device_id
        guard active else { throw CancellationError() }
        try await identity.save(deviceID: id)
        return id
    }
}

enum LoopbackSignature {
    static func message(method: String, target: String, timestamp: String, body: Data) -> Data {
        Data("\(method.uppercased())\n\(target)\n\(timestamp)\n\(LoopbackDigest.hex(body))".utf8)
    }
}
