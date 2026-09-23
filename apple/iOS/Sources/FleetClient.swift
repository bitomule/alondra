import AlondraShared
import Foundation

struct FleetClient: Sendable {
    let base: URL

    static let defaultURL = "http://127.0.0.1:47800"

    init?(string: String) {
        let trimmed = string.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed), let scheme = url.scheme, ["http", "https"].contains(scheme), url.host() != nil else {
            return nil
        }
        base = url
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

    func overview() async throws -> Overview {
        let data = try await get("v1/overview")
        return try Self.decoder.decode(Overview.self, from: data)
    }

    func waitForChange(after tick: UInt64) async throws {
        _ = try await get("v1/changes", query: ["after": String(tick), "wait": "50"], session: Self.longPoll)
    }

    func health() async throws {
        _ = try await get("v1/health")
    }

    func file(_ storedPath: String) async throws -> String {
        let data = try await get("v1/file", query: ["path": storedPath])
        return String(decoding: data, as: UTF8.self)
    }

    func answer(_ question: UInt64, text: String, option: String?) async throws {
        var body = ["text": text, "by": "David", "device": "iphone"]
        body["option"] = option
        try await post("v1/questions/\(question)/answer", body)
    }

    func moreContext(_ question: UInt64, note: String) async throws {
        try await post("v1/questions/\(question)/more-context", ["note": note])
    }

    func markRead(_ report: UInt64) async throws {
        try await post("v1/reports/\(report)/read", [:])
    }

    func send(to session: String, text: String) async throws {
        try await post("v1/nodes/\(session)/messages", ["text": text, "from": "david"])
    }

    func newRequest(_ text: String) async throws {
        try await post("v1/requests", ["text": text])
    }

    private func get(_ path: String, query: [String: String] = [:], session: URLSession = session) async throws -> Data {
        var url = base.appending(path: path)
        if !query.isEmpty {
            url.append(queryItems: query.sorted { $0.key < $1.key }.map { URLQueryItem(name: $0.key, value: $0.value) })
        }
        let (data, response) = try await session.data(from: url)
        try Self.check(data, response)
        return data
    }

    private func post(_ path: String, _ body: [String: String]) async throws {
        var request = URLRequest(url: base.appending(path: path))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, response) = try await Self.session.data(for: request)
        try Self.check(data, response)
    }

    private static func check(_ data: Data, _ response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse, http.statusCode >= 400 else { return }
        let message = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?["error"] as? String
        throw FleetError.server(message ?? "El servidor respondió \(http.statusCode)")
    }
}

enum FleetError: LocalizedError {
    case server(String)

    var errorDescription: String? {
        switch self {
        case .server(let message): message
        }
    }
}

extension Error {
    var isCancellation: Bool {
        if self is CancellationError { return true }
        return (self as? URLError)?.code == .cancelled
    }

    var friendlyDescription: String {
        guard let urlError = self as? URLError else { return localizedDescription }
        switch urlError.code {
        case .cannotConnectToHost, .cannotFindHost: return "No encuentro el servidor de Alondra"
        case .notConnectedToInternet, .networkConnectionLost: return "Sin conexión"
        case .timedOut: return "El servidor no contesta"
        default: return urlError.localizedDescription
        }
    }
}
