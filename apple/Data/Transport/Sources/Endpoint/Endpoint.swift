import Foundation

public enum HTTPMethod: String, Sendable {
    case get = "GET"
    case post = "POST"
    case patch = "PATCH"
    case put = "PUT"
    case delete = "DELETE"
}

public struct Endpoint<Response: Sendable>: Sendable {
    public let method: HTTPMethod
    public let path: [String]
    public let query: [(String, String)]
    public let body: Data?
    public let contentType: String?
    public let requiresJSONObjectResponse: Bool
    public let decode: @Sendable (Data) throws -> Response

    public init(
        _ method: HTTPMethod,
        path: [String],
        query: [(String, String)] = [],
        body: Data? = nil,
        contentType: String? = nil,
        requiresJSONObjectResponse: Bool = false,
        decode: @escaping @Sendable (Data) throws -> Response
    ) {
        self.method = method
        self.path = path
        self.query = query
        self.body = body
        self.contentType = contentType
        self.requiresJSONObjectResponse = requiresJSONObjectResponse
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
        requiresJSONObjectResponse: Bool = false
    ) -> Self {
        .init(method, path: path, query: query, body: body, contentType: contentType, requiresJSONObjectResponse: requiresJSONObjectResponse) { $0 }
    }
}

extension Endpoint where Response: Decodable {
    public static func json(
        _ method: HTTPMethod,
        path: [String],
        query: [(String, String)] = [],
        body: Data? = nil
    ) -> Self {
        .init(method, path: path, query: query, body: body, contentType: body == nil ? nil : "application/json") { data in
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            return try decoder.decode(Response.self, from: data)
        }
    }
}
