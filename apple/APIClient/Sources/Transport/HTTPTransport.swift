import Foundation

public struct HTTPResponse: Sendable {
    public let status: Int
    public let body: Data
    public let url: URL?

    public init(status: Int, body: Data, url: URL? = nil) {
        self.status = status
        self.body = body
        self.url = url
    }
}

public protocol HTTPTransport: Sendable {
    func send(_ request: URLRequest, policy: RequestPolicy) async throws -> HTTPResponse
    func upload(_ request: URLRequest, body: Data, progress: @escaping @Sendable (Double) -> Void) async throws -> HTTPResponse
}
