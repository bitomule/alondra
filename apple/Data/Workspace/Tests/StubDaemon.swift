import Foundation

/// An in-process stand-in for alondrad: every request the session sends is recorded, and the
/// test decides the reply.
final class StubDaemon: @unchecked Sendable {
    enum Reply {
        case status(Int, Data)
        case failure(URLError.Code)
    }

    struct Received {
        let method: String
        let target: String
        let headers: [String: String]
        let body: Data
    }

    private let lock = NSLock()
    private var received: [Received] = []
    private var handler: (Received) -> Reply

    init(handler: @escaping (Received) -> Reply) { self.handler = handler }

    var requests: [Received] { lock.withLock { received } }

    func replace(_ handler: @escaping (Received) -> Reply) { lock.withLock { self.handler = handler } }

    func respond(to request: URLRequest) -> Reply {
        let url = request.url!
        let target = url.path(percentEncoded: true) + (url.query(percentEncoded: true).map { "?" + $0 } ?? "")
        let item = Received(method: request.httpMethod ?? "GET", target: target,
                            headers: request.allHTTPHeaderFields ?? [:], body: Self.body(of: request))
        return lock.withLock {
            received.append(item)
            return handler(item)
        }
    }

    private static func body(of request: URLRequest) -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open()
        defer { stream.close() }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 16 * 1024)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: buffer.count)
            guard read > 0 else { break }
            data.append(buffer, count: read)
        }
        return data
    }

    static let active = Registry()

    final class Registry: @unchecked Sendable {
        private let lock = NSLock()
        private var daemon: StubDaemon?
        var current: StubDaemon? {
            get { lock.withLock { daemon } }
            set { lock.withLock { daemon = newValue } }
        }
    }

    func urlSession() -> URLSession {
        Self.active.current = self
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        return URLSession(configuration: configuration)
    }
}

final class StubURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        guard let daemon = StubDaemon.active.current else {
            client?.urlProtocol(self, didFailWithError: URLError(.cannotConnectToHost))
            return
        }
        switch daemon.respond(to: request) {
        case .failure(let code):
            client?.urlProtocol(self, didFailWithError: URLError(code))
        case let .status(status, body):
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: nil)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: body)
            client?.urlProtocolDidFinishLoading(self)
        }
    }
}
