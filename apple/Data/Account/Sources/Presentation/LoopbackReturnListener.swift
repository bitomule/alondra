import Foundation
import Network

/// Listens on this device's own `localhost:<port>` while an MCP sign-in is open, so the browser's
/// return to `http://localhost:<port>/callback` lands here instead of on a page that does not load.
/// The address it hands back carries a one-time code: it lives only in memory, is never written
/// down or logged, and the listener closes as soon as it has it.
public final class LoopbackReturnListener: Sendable {
    public enum Failure: Error, Equatable, Sendable {
        /// Something else on this device holds the port, such as Claude's own listener on the Mac.
        case portTaken
        case unavailable
    }

    private let listeners: [NWListener]
    private let queue = DispatchQueue(label: "alondra.mcp-return")
    private let returns: AsyncStream<URL>
    private let deliver: AsyncStream<URL>.Continuation
    public let port: UInt16

    private init(port: UInt16, listeners: [NWListener], returns: AsyncStream<URL>, deliver: AsyncStream<URL>.Continuation) {
        self.port = port
        self.listeners = listeners
        self.returns = returns
        self.deliver = deliver
    }

    /// Starts listening on `127.0.0.1` and `::1`; IPv6 is best effort, IPv4 must succeed.
    public static func open(port: UInt16, page: @escaping @Sendable () -> String) async throws -> LoopbackReturnListener {
        guard let endpointPort = NWEndpoint.Port(rawValue: port), port != 0 else { throw Failure.unavailable }
        let (returns, deliver) = AsyncStream.makeStream(of: URL.self, bufferingPolicy: .bufferingNewest(1))
        var listeners: [NWListener] = []
        for host in ["127.0.0.1", "::1"] {
            let parameters = NWParameters.tcp
            parameters.acceptLocalOnly = true
            parameters.allowLocalEndpointReuse = false
            parameters.requiredLocalEndpoint = .hostPort(host: NWEndpoint.Host(host), port: endpointPort)
            guard let listener = try? NWListener(using: parameters) else {
                if host == "127.0.0.1" { throw Failure.unavailable }
                continue
            }
            listeners.append(listener)
        }
        let made = LoopbackReturnListener(port: port, listeners: listeners, returns: returns, deliver: deliver)
        var ready = 0
        for (index, listener) in listeners.enumerated() {
            do {
                try await made.start(listener, page: page)
                ready += 1
            } catch {
                listener.cancel()
                if index == 0 {
                    made.close()
                    throw error
                }
            }
        }
        guard ready > 0 else { throw Failure.unavailable }
        return made
    }

    /// The first address the browser came back to, or nil once the listener is closed.
    public func next() async -> URL? {
        for await address in returns { return address }
        return nil
    }

    public func close() {
        for listener in listeners { listener.cancel() }
        deliver.finish()
    }

    private func start(_ listener: NWListener, page: @escaping @Sendable () -> String) async throws {
        let port = port
        let deliver = deliver
        let queue = queue
        listener.newConnectionHandler = { connection in
            connection.start(queue: queue)
            Self.read(connection, port: port, page: page, deliver: deliver)
        }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
            let once = ResumeOnce(continuation)
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready: once.resume(with: .success(()))
                case let .failed(error), let .waiting(error):
                    if case let .posix(code) = error, code == .EADDRINUSE {
                        once.resume(with: .failure(Failure.portTaken))
                    } else {
                        once.resume(with: .failure(Failure.unavailable))
                    }
                case .cancelled: once.resume(with: .failure(Failure.unavailable))
                default: break
                }
            }
            listener.start(queue: queue)
        }
    }

    private static func read(_ connection: NWConnection, port: UInt16, page: @escaping @Sendable () -> String,
                             deliver: AsyncStream<URL>.Continuation, head: Data = Data()) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 8192) { data, _, done, error in
            var head = head
            if let data { head.append(data) }
            guard error == nil, head.count <= 16 * 1024 else { return connection.cancel() }
            guard let end = head.range(of: Data("\r\n\r\n".utf8)) else {
                if done { connection.cancel() } else { read(connection, port: port, page: page, deliver: deliver, head: head) }
                return
            }
            let address = Self.address(requestHead: head[..<end.lowerBound], port: port)
            let answer = address == nil ? Self.response(status: "404 Not Found", body: "")
                                        : Self.response(status: "200 OK", body: page())
            connection.send(content: answer, completion: .contentProcessed { _ in connection.cancel() })
            if let address { deliver.yield(address) }
        }
    }

    /// `http://localhost:<port>/callback?…` for a request to `/callback` that carries the
    /// sign-in's `state` and its `code` or `error`; nil for anything else, such as a favicon.
    public static func address(requestHead: Data, port: UInt16) -> URL? {
        guard let line = String(data: requestHead, encoding: .utf8)?.split(separator: "\r\n", maxSplits: 1).first else { return nil }
        let parts = line.split(separator: " ")
        guard parts.count == 3, parts[0] == "GET", parts[1].hasPrefix("/"), parts[2].hasPrefix("HTTP/1."),
              let components = URLComponents(string: "http://localhost:\(port)" + parts[1]),
              components.path == "/callback", components.fragment == nil,
              let items = components.queryItems, items.contains(where: { $0.name == "state" }),
              items.contains(where: { $0.name == "code" || $0.name == "error" }) else { return nil }
        return components.url
    }

    private static func response(status: String, body: String) -> Data {
        let bytes = Data(body.utf8)
        let head = "HTTP/1.1 \(status)\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: \(bytes.count)\r\n"
            + "Cache-Control: no-store\r\nConnection: close\r\n\r\n"
        return Data(head.utf8) + bytes
    }
}

private final class ResumeOnce: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, any Error>?

    init(_ continuation: CheckedContinuation<Void, any Error>) { self.continuation = continuation }

    func resume(with result: Result<Void, any Error>) {
        lock.lock()
        let pending = continuation
        continuation = nil
        lock.unlock()
        pending?.resume(with: result)
    }
}
