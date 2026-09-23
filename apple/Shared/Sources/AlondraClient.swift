import Foundation

struct AlondraClient: Sendable {
    let base: URL

    static let local = AlondraClient(base: URL(string: "http://127.0.0.1:47800")!)

    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    private static let longSession: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 70
        return URLSession(configuration: config)
    }()

    func overview() async throws -> Overview {
        let (data, _) = try await URLSession.shared.data(from: base.appending(path: "v1/overview"))
        return try Self.decoder.decode(Overview.self, from: data)
    }

    func waitForChange(after tick: UInt64) async throws {
        var components = URLComponents(url: base.appending(path: "v1/changes"), resolvingAgainstBaseURL: false)!
        components.queryItems = [URLQueryItem(name: "after", value: String(tick)), URLQueryItem(name: "wait", value: "50")]
        _ = try await Self.longSession.data(from: components.url!)
    }

    func answer(_ question: UInt64, text: String, option: String?) async throws {
        var body: [String: String] = ["text": text, "by": "David", "device": "mac"]
        if let option { body["option"] = option }
        try await post("v1/questions/\(question)/answer", body)
    }

    func acknowledge(_ session: String) async throws {
        try await post("v1/nodes/\(session)/ack", [:])
    }

    func send(to session: String, text: String) async throws {
        try await post("v1/nodes/\(session)/messages", ["text": text, "from": "david"])
    }

    private func post(_ path: String, _ body: [String: String]) async throws {
        var request = URLRequest(url: base.appending(path: path))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, response) = try await URLSession.shared.data(for: request)
        if let http = response as? HTTPURLResponse, http.statusCode >= 400 {
            let message = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])?["error"] as? String
            throw AlondraError.server(message ?? "HTTP \(http.statusCode)")
        }
    }
}

enum AlondraError: LocalizedError {
    case server(String)

    var errorDescription: String? {
        switch self {
        case .server(let message): message
        }
    }
}
