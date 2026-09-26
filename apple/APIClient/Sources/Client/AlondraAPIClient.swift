import Foundation
import OSLog

public struct AlondraAPIClient: Sendable {
    public let base: URL
    public let device: String
    public let signer: (any RequestSigner)?
    private let transport: (any HTTPTransport)?
    let operations: (any ServiceOperationTransport)?
    private let now: @Sendable () -> Date

    @available(*, deprecated, message: "Migration only: use the service initializer. Remove direct daemon access with app wiring.")
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
        self.operations = nil
        self.now = now
    }

    public init(serviceURL: URL, deviceID: String, operations: any ServiceOperationTransport) {
        self.base = serviceURL
        self.device = deviceID
        self.signer = nil
        self.transport = nil
        self.operations = operations
        self.now = { Date() }
    }

    public func send<Response>(_ endpoint: Endpoint<Response>, operationID: String? = nil) async throws -> Response {
        if let operations {
            guard endpoint.method == .get || operationID?.isEmpty == false else {
                throw ServiceAPIError.operationIdentityRequired
            }
            let request = try ServiceRequest(endpoint: endpoint, operationID: operationID ?? UUID().uuidString)
            try Task.checkCancellation()
            switch try await operations.perform(request) {
            case .unknown: throw ServiceAPIError.outcomeUnknown
            case let .completed(status, body):
                guard (200..<300).contains(status) else { throw APIError.from(status: status, body: body) }
                return try endpoint.decode(body)
            }
        }
        guard let transport else { throw ServiceAPIError.invalidConfiguration }

        let request = try makeRequest(endpoint)
        try Task.checkCancellation()
        let response = try await transport.send(request, policy: endpoint.policy)
        try Task.checkCancellation()
        try Self.check(response)
        return try endpoint.decode(response.body)
    }

    public func makeRequest<Response>(_ endpoint: Endpoint<Response>) throws -> URLRequest {
        guard operations == nil else { throw ServiceAPIError.directRequestUnavailable }
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
        guard let transport else { throw ServiceAPIError.uploadUnsupported }
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
