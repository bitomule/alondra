import Foundation

public enum HTTPMethod: String, Sendable {
    case get = "GET"
    case post = "POST"
    case put = "PUT"
    case delete = "DELETE"
}

public struct Endpoint<Response: Sendable>: Sendable {
    public let method: HTTPMethod
    public let path: [String]
    public let query: [(String, String)]
    public let body: Data?
    public let contentType: String?
    public let policy: RequestPolicy
    public let decode: @Sendable (Data) throws -> Response

    public init(
        _ method: HTTPMethod,
        path: [String],
        query: [(String, String)] = [],
        body: Data? = nil,
        contentType: String? = nil,
        policy: RequestPolicy = .normal,
        decode: @escaping @Sendable (Data) throws -> Response
    ) {
        self.method = method
        self.path = path
        self.query = query
        self.body = body
        self.contentType = contentType
        self.policy = policy
        self.decode = decode
    }
}

extension Endpoint where Response == Data {
    public static func data(
        _ method: HTTPMethod,
        path: [String],
        query: [(String, String)] = [],
        body: Data? = nil,
        contentType: String? = nil,
        policy: RequestPolicy = .normal
    ) -> Self {
        .init(method, path: path, query: query, body: body, contentType: contentType, policy: policy) { $0 }
    }
}

extension Endpoint where Response: Decodable {
    public static func json(
        _ method: HTTPMethod,
        path: [String],
        query: [(String, String)] = [],
        body: Data? = nil,
        policy: RequestPolicy = .normal
    ) -> Self {
        .init(method, path: path, query: query, body: body, contentType: body == nil ? nil : "application/json", policy: policy) { data in
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            return try decoder.decode(Response.self, from: data)
        }
    }
}
