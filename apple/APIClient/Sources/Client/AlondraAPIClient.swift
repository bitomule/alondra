import Foundation
import OSLog

public struct AlondraAPIClient: Sendable {
    public let base: URL
    public let device: String
    public let signer: (any RequestSigner)?
    public let transport: any HTTPTransport
    private let now: @Sendable () -> Date

    public init(
        base: URL,
        device: String = "mac",
        signer: (any RequestSigner)? = nil,
        transport: any HTTPTransport = URLSessionTransport(),
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.base = base
        self.device = device
        self.signer = signer
        self.transport = transport
        self.now = now
    }

    public func send<Response>(_ endpoint: Endpoint<Response>) async throws -> Response {
        let request = try makeRequest(endpoint)
        try Task.checkCancellation()
        let response = try await transport.send(request, policy: endpoint.policy)
        try Task.checkCancellation()
        try Self.check(response)
        return try endpoint.decode(response.body)
    }

    public func makeRequest<Response>(_ endpoint: Endpoint<Response>) throws -> URLRequest {
        var url = base
        for segment in endpoint.path { url.append(component: segment) }
        if !endpoint.query.isEmpty {
            url.append(queryItems: endpoint.query.map { URLQueryItem(name: $0.0, value: $0.1) })
        }
        var request = URLRequest(url: url)
        request.httpMethod = endpoint.method.rawValue
        request.httpBody = endpoint.body
        if let contentType = endpoint.contentType { request.setValue(contentType, forHTTPHeaderField: "Content-Type") }
        if let signer { try RequestSignature.sign(&request, with: signer, now: now()) }
        return request
    }

    public func upload(_ data: Data, name: String, progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws -> String {
        let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
        var url = base.appending(path: "v1/uploads")
        var components = URLComponents(url: url, resolvingAgainstBaseURL: false)!
        components.percentEncodedQuery = "name=" + (name.addingPercentEncoding(withAllowedCharacters: unreserved) ?? "attachment")
        url = components.url!
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        if let signer { try RequestSignature.sign(&request, with: signer, now: now(), body: data) }
        try Task.checkCancellation()
        let response = try await transport.upload(request, body: data, progress: progress)
        try Task.checkCancellation()
        try Self.check(response)
        struct Stored: Decodable { let id: String }
        return try JSONDecoder().decode(Stored.self, from: response.body).id
    }

    private static let log = Logger(subsystem: "com.bitomule.alondra", category: "api-client")

    private static func check(_ response: HTTPResponse) throws {
        guard !(200..<300).contains(response.status) else { return }
        let error = APIError.from(status: response.status, body: response.body)
        if case .server = error {} else {
            log.error("HTTP \(response.status, privacy: .public) from \(response.url?.path() ?? "", privacy: .public): \(String(decoding: response.body.prefix(2000), as: UTF8.self), privacy: .private)")
        }
        throw error
    }
}
