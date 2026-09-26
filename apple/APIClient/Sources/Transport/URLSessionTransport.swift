import Foundation

public struct URLSessionTransport: HTTPTransport {
    private let normal: URLSession
    private let merge: URLSession
    private let longPoll: URLSession
    private let uploads: URLSession

    public init() {
        func session(_ timeout: TimeInterval) -> URLSession {
            let config = URLSessionConfiguration.default
            config.timeoutIntervalForRequest = timeout
            config.waitsForConnectivity = false
            return URLSession(configuration: config)
        }
        normal = session(RequestPolicy.normal.timeout)
        merge = session(RequestPolicy.merge.timeout)
        longPoll = session(RequestPolicy.longPoll.timeout)
        uploads = session(RequestPolicy.upload.timeout)
    }

    public func send(_ request: URLRequest, policy: RequestPolicy) async throws -> HTTPResponse {
        let session: URLSession
        switch policy {
        case .normal: session = normal
        case .merge: session = merge
        case .longPoll: session = longPoll
        case .upload: session = uploads
        }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        return HTTPResponse(status: http.statusCode, body: data, url: response.url)
    }

    public func upload(_ request: URLRequest, body: Data, progress: @escaping @Sendable (Double) -> Void) async throws -> HTTPResponse {
        let (data, response) = try await uploads.upload(for: request, from: body, delegate: UploadProgress(progress))
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        return HTTPResponse(status: http.statusCode, body: data, url: response.url)
    }
}

private final class UploadProgress: NSObject, URLSessionTaskDelegate, Sendable {
    let report: @Sendable (Double) -> Void

    init(_ report: @escaping @Sendable (Double) -> Void) { self.report = report }

    func urlSession(_ session: URLSession, task: URLSessionTask, didSendBodyData bytesSent: Int64, totalBytesSent: Int64, totalBytesExpectedToSend: Int64) {
        guard totalBytesExpectedToSend > 0 else { return }
        report(Double(totalBytesSent) / Double(totalBytesExpectedToSend))
    }
}
