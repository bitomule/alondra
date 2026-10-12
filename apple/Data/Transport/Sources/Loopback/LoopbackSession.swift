import CryptoKit
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
public actor LoopbackSession: OperationAPI {
    /// `ALONDRA_PORT` points the app at a sandbox daemon from `make e2e-sandbox`.
    public static let defaultBase: URL = {
        let port = ProcessInfo.processInfo.environment["ALONDRA_PORT"].flatMap(UInt16.init) ?? 47800
        return URL(string: "http://127.0.0.1:\(port)")!
    }()

    public nonisolated let base: URL
    let urlSession: URLSession
    private let identity: LoopbackIdentityStore
    private let deviceName: String
    private let now: @Sendable () -> Date
    private let requestClock: RequestClock
    let mutations: MutationRunner
    private let mutationEvidence = MutationEvidenceStore()
    private let requestTransport: (any HTTPTransport)?
    private var pairing: Task<String, Error>?
    private var outcomes: [String: LoopbackOutcome] = [:]
    var uploads: [String: LoopbackUpload] = [:]
    var downloads: [String: LoopbackDownload] = [:]
    private var active = true

    public init(base: URL = LoopbackSession.defaultBase, identity: LoopbackIdentityStore, deviceName: String,
                urlSession: URLSession = LoopbackSession.makeURLSession(), now: @escaping @Sendable () -> Date = { .now }) {
        self.init(base: base, identity: identity, deviceName: deviceName, urlSession: urlSession,
            now: now, requestClock: .continuous)
    }

    init(base: URL = LoopbackSession.defaultBase, identity: LoopbackIdentityStore, deviceName: String,
         urlSession: URLSession = LoopbackSession.makeURLSession(), now: @escaping @Sendable () -> Date = { .now },
         requestClock: RequestClock, transport: (any HTTPTransport)? = nil) {
        self.requestClock = requestClock
        mutations = MutationRunner(clock: requestClock)
        self.requestTransport = transport
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

    /// This Mac's own ID, read from its daemon's direct port. Unsigned and public, like `hello`.
    public static func ownMacID(at base: URL = defaultBase, urlSession: URLSession = makeURLSession()) async -> String? {
        struct Daemon: Decodable {
            struct Direct: Decodable { let port: UInt16 }
            let direct: Direct?
        }
        let request = URLRequest(url: base.appending(path: "daemon"), timeoutInterval: 2)
        guard let (body, _) = try? await urlSession.data(for: request),
              let port = (try? JSONDecoder().decode(Daemon.self, from: body))?.direct?.port else { return nil }
        let http = DirectHTTP(transport: URLSessionTransport())
        return try? await http.hello(at: DirectEndpoint(host: "127.0.0.1", port: port, kind: .localNetwork)).macID
    }

    public func deactivate() async {
        active = false
        await mutations.cancelAll()
        pairing?.cancel()
        pairing = nil
    }

    /// The last known result of a mutation sent by this process. Anything else is unknown:
    /// without a durable outbox, absence is never proof that nothing was sent.
    public func outcome(requestID: String) -> LoopbackOutcome {
        outcomes[requestID] ?? .unknown
    }

    public func recover(_ request: APIRequest) async throws -> APIResponse {
        guard active, request.method == "POST", request.path == "/actions", let body = request.body,
              let object = try JSONSerialization.jsonObject(with: body) as? [String: Any],
              let epoch = object["data_epoch"] as? String else { return .unknown }
        let digest = SHA256.hash(data: body).map { String(format: "%02x", $0) }.joined()
        let read = try APIRequest(endpoint: Endpoint<Data>.data(.get,
            path: ["operation-results", request.operationID], query: [("digest", digest), ("data_epoch", epoch)]),
            operationID: UUID().uuidString)
        guard case let .completed(status, bytes) = try await perform(read), status == 200 else { return .unknown }
        guard let result = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              result["request_id"] as? String == request.operationID,
              result["digest"] as? String == digest, result["data_epoch"] as? String == epoch,
              let response = result["response"] as? [String: Any] else { throw LoopbackSessionError.invalidResponse }
        return .completed(status: 200, body: try JSONSerialization.data(withJSONObject: response, options: [.sortedKeys]))
    }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        guard request.method != "GET" else { return try await performPrepared(request) }
        let startedAt = requestClock.now()
        let owner = MutationScope.current?.id ?? UUID()
        let lease = mutationEvidence.acquire(request.operationID, owner: owner)
        defer { mutationEvidence.release(lease) }
        let result = try await mutations.run(dispatchEvidence: lease.evidence, ownerID: owner, startedAt: startedAt, unknown: { APIResponse.unknown }) {
            try await self.performPrepared(request)
        }
        if result == .unknown {
            if outcomes[request.operationID] == nil { outcomes[request.operationID] = .unknown }
            mutationEvidence.retain(request.operationID)
        }
        return result
    }

    private func performPrepared(_ request: APIRequest) async throws -> APIResponse {
        try MutationScope.check()
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
            try MutationScope.check()
            let device = try await pairedDeviceID()
            try MutationScope.check()
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
                try MutationScope.check()
                repaired = true
                await forget(device: device)
                continue
            }
            let outcome = LoopbackOutcome.completed(status: status, body: data)
            if !isRead, let operationID {
                outcomes[operationID] = outcome
                mutationEvidence.retain(operationID)
            }
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
        var request = request
        request.timeoutInterval = try MutationScope.remaining(cap: .seconds(request.timeoutInterval)).requestTimeInterval
        try MutationScope.check()
        if request.httpMethod != "GET" { try MutationScope.possibleDispatch() }
        if let requestTransport {
            let reply = try await requestTransport.send(request)
            return (reply.status, reply.body)
        }
        let (data, response) = try await urlSession.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw LoopbackSessionError.invalidResponse }
        return (http.statusCode, data)
    }

    private func signedRequest(method: String, target: String, body: Data?, device: String,
                               contentType: String, timeout: TimeInterval) async throws -> URLRequest {
        guard target.hasPrefix("/"), let url = URL(string: base.absoluteString + target) else {
            throw OperationError.invalidEndpoint
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
        let task = MutationScope.$current.withValue(nil) { Task { try await self.pair() } }
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
