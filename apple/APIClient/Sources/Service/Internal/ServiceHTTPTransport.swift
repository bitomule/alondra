import Foundation

public struct ServiceHTTPResponse: Sendable {
    public let status: Int
    public let body: Data

    public init(status: Int, body: Data) { self.status = status; self.body = body }
}

public protocol ServiceHTTPTransport: Sendable {
    func send(_ request: URLRequest) async throws -> ServiceHTTPResponse
}

/// URLSession's automatic redirects could forward a bearer token to another origin.
/// Sendable safety: the delegate has no stored mutable state; callbacks only reject redirects.
private final class NoRedirects: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

actor URLSessionServiceTransport: ServiceHTTPTransport {
    private let session: URLSession

    public init() {
        let config = URLSessionConfiguration.ephemeral
        config.httpCookieStorage = nil
        config.urlCache = nil
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        session = URLSession(configuration: config, delegate: NoRedirects(), delegateQueue: nil)
    }

    public func send(_ request: URLRequest) async throws -> ServiceHTTPResponse {
        let (body, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw RelayWireError.invalidResponse }
        return ServiceHTTPResponse(status: http.statusCode, body: body)
    }
}
