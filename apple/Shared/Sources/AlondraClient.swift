import Foundation

public struct AlondraClient: Sendable {
    public let base: URL
    public let device: String
    public let signer: (any RequestSigner)?

    public static let defaultURL = URL(string: "http://127.0.0.1:47800")!
    public static let local = AlondraClient(base: defaultURL)

    public init(base: URL, device: String = "mac", signer: (any RequestSigner)? = nil) {
        self.base = base
        self.device = device
        self.signer = signer
    }

    public init?(string: String, device: String = "mac", signer: (any RequestSigner)? = nil) {
        guard let url = Self.serverURL(string) else { return nil }
        self.init(base: url, device: device, signer: signer)
    }

    public static func serverURL(_ string: String) -> URL? {
        let trimmed = string.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed), let scheme = url.scheme, ["http", "https"].contains(scheme), url.host() != nil else {
            return nil
        }
        return url
    }

    public var isLoopback: Bool {
        Self.isLoopback(base)
    }

    public static func isLoopback(_ url: URL) -> Bool {
        guard let host = url.host()?.lowercased() else { return false }
        return host == "localhost" || host == "::1" || host == "[::1]" || host.hasPrefix("127.")
    }

    public var identity: String {
        "\(base.absoluteString)#\(signer?.deviceID ?? "")"
    }

    private static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    private static let session: URLSession = {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 15
        config.waitsForConnectivity = false
        return URLSession(configuration: config)
    }()

    private static let longPoll: URLSession = {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 70
        return URLSession(configuration: config)
    }()

    public func health() async throws {
        _ = try await get("v1/health")
    }

    public func overview() async throws -> Overview {
        try Self.decoder.decode(Overview.self, from: try await get("v1/overview"))
    }

    public func waitForChange(after tick: UInt64) async throws {
        _ = try await get("v1/changes", query: [("after", String(tick)), ("wait", "50")], session: Self.longPoll)
    }

    public func answer(_ question: UInt64, text: String, option: String?) async throws {
        var body = ["text": text, "device": device]
        body["option"] = option
        try await post("v1/questions/\(question)/answer", body)
    }

    public func moreContext(_ question: UInt64, note: String) async throws {
        try await post("v1/questions/\(question)/more-context", ["note": note])
    }

    public func mergePullRequest(_ id: UInt64, method: String) async throws {
        try await post("v1/prs/\(id)/merge", ["method": method])
    }

    public func requestChanges(onPullRequest id: UInt64, text: String) async throws {
        try await post("v1/prs/\(id)/changes", ["text": text])
    }

    public func dismissPullRequest(_ id: UInt64) async throws {
        try await post("v1/prs/\(id)/dismiss", [:])
    }

    public func activity(client: String) async throws {
        try await post("v1/activity", ["client": client])
    }

    public func newRequest(_ text: String) async throws {
        try await post("v1/requests", ["text": text])
    }

    public func search(_ query: String) async throws -> SearchResult {
        try Self.decoder.decode(SearchResult.self, from: try await get("v1/search", query: [("q", query)]))
    }

    public func fileText(_ path: String) async throws -> String {
        String(decoding: try await get("v1/file", query: [("path", path)]), as: UTF8.self)
    }

    public func markRead(_ report: UInt64) async throws {
        try await post("v1/reports/\(report)/read", [:])
    }

    public func closeNode(_ key: String) async throws {
        let url = base.appending(path: "v1/nodes").appending(component: key).appending(path: "close")
        do {
            _ = try await perform("POST", url: url, body: Data("{}".utf8), session: Self.longPoll)
        } catch AlondraError.server(let message) {
            throw CloseRefused(message: message)
        }
    }

    public func readAndClose(_ report: Report) async throws {
        try await closeNode(report.sessionId)
        try await markRead(report.id)
    }

    public func acknowledge(_ session: String) async throws {
        try await post("v1/nodes/\(session)/ack", [:])
    }

    public func send(to session: String, text: String) async throws {
        try await post("v1/nodes/\(session)/messages", ["text": text, "from": "david"])
    }

    public func registerPush(_ registration: PushRegistration) async throws {
        _ = try await perform("POST", "v1/push/register", body: try JSONEncoder().encode(registration), session: Self.session)
    }

    public static func pair(at base: URL, code: String, deviceName: String, publicKey: Data) async throws -> String {
        struct Paired: Decodable { let deviceId: String }
        let body = try JSONSerialization.data(withJSONObject: [
            "code": code,
            "device_name": deviceName,
            "public_key": publicKey.base64EncodedString(),
        ])
        let data = try await AlondraClient(base: base).perform("POST", "v1/pair", body: body, session: session)
        return try decoder.decode(Paired.self, from: data).deviceId
    }

    public func createPairing() async throws -> PairingInvite {
        let data = try await perform("POST", "v1/pairing", body: Data("{}".utf8), session: Self.session)
        return try Self.decoder.decode(PairingInvite.self, from: data)
    }

    public func pairedDevices() async throws -> [PairedDevice] {
        struct List: Decodable { let devices: [PairedDevice] }
        return try Self.decoder.decode(List.self, from: try await get("v1/devices")).devices
    }

    public func revokeDevice(_ id: String) async throws {
        _ = try await perform("DELETE", "v1/devices/\(id)", session: Self.session)
    }

    private func get(_ path: String, query: [(String, String)] = [], session: URLSession = session) async throws -> Data {
        try await perform("GET", path, query: query, session: session)
    }

    private func post(_ path: String, _ body: [String: String]) async throws {
        _ = try await perform("POST", path, body: try JSONSerialization.data(withJSONObject: body), session: Self.session)
    }

    private func perform(_ method: String, _ path: String, query: [(String, String)] = [], body: Data? = nil, session: URLSession) async throws -> Data {
        try await perform(method, url: base.appending(path: path), query: query, body: body, session: session)
    }

    private func perform(_ method: String, url: URL, query: [(String, String)] = [], body: Data? = nil, session: URLSession) async throws -> Data {
        var url = url
        if !query.isEmpty {
            url.append(queryItems: query.map { URLQueryItem(name: $0.0, value: $0.1) })
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if let signer {
            try RequestSignature.sign(&request, with: signer)
        }
        let (data, response) = try await session.data(for: request)
        try Self.check(data, response)
        return data
    }

    private static func check(_ data: Data, _ response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse, http.statusCode >= 400 else { return }
        if http.statusCode == 401 { throw AlondraError.unauthorized }
        let json = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?["error"] as? String
        let text = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        let message = json ?? (text.isEmpty || text.hasPrefix("{") ? nil : text)
        throw AlondraError.server(message ?? String(localized: "The server responded \(http.statusCode)", table: "AlondraShared", bundle: #bundle))
    }
}

public struct PairingInvite: Decodable, Sendable {
    public let code: String
    public let expiresIn: Int
    public let url: String?

    public func payload(fallbackURL: String) -> String {
        let object = ["url": url ?? fallbackURL, "code": code]
        let data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])) ?? Data()
        return String(decoding: data, as: UTF8.self)
    }
}

public struct PairedDevice: Decodable, Identifiable, Hashable, Sendable {
    public let id: String
    public let name: String
    public let createdAt: Int64
    public let lastSeen: Int64?
}

public struct PushRegistration: Encodable, Hashable, Sendable {
    public enum Environment: String, Encodable, Sendable {
        case sandbox, production
    }

    public let token: String
    public let environment: Environment
    public let quietStart: String?
    public let quietEnd: String?

    public init(token: String, environment: Environment, quietStart: String?, quietEnd: String?) {
        self.token = token
        self.environment = environment
        self.quietStart = quietStart
        self.quietEnd = quietEnd
    }

    private enum CodingKeys: String, CodingKey {
        case token, environment
        case quietStart = "quiet_start"
        case quietEnd = "quiet_end"
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(token, forKey: .token)
        try container.encode(environment, forKey: .environment)
        try container.encode(quietStart, forKey: .quietStart)
        try container.encode(quietEnd, forKey: .quietEnd)
    }
}

public enum AlondraError: LocalizedError, Equatable {
    case server(String)
    case unauthorized

    public var errorDescription: String? {
        switch self {
        case .server(let message): message
        case .unauthorized: String(localized: "This device is not paired", table: "AlondraShared", bundle: #bundle)
        }
    }
}

public struct CloseRefused: LocalizedError, Equatable {
    public let message: String

    public var errorDescription: String? { message }
}

extension Error {
    public var isCancellation: Bool {
        if self is CancellationError { return true }
        return (self as? URLError)?.code == .cancelled
    }

    public var friendlyDescription: String {
        guard let urlError = self as? URLError else { return localizedDescription }
        switch urlError.code {
        case .cannotConnectToHost, .cannotFindHost: return String(localized: "Can’t find the Alondra server", table: "AlondraShared", bundle: #bundle)
        case .notConnectedToInternet, .networkConnectionLost: return String(localized: "No connection", table: "AlondraShared", bundle: #bundle)
        case .timedOut: return String(localized: "The server isn’t responding", table: "AlondraShared", bundle: #bundle)
        default: return urlError.localizedDescription
        }
    }
}
