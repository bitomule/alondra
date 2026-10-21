#if os(iOS) || os(macOS)
import AlondraFeatures
import AuthenticationServices
import Foundation
#if os(iOS)
import UIKit
#else
import AppKit
#endif

/// Opens an MCP server's sign-in page and, while it is open, listens on this device's own
/// `localhost:<port>` for the browser's return. On the iPhone the page opens in the system's
/// sign-in sheet, which stays over the app so the app keeps listening; on the Mac it opens in the
/// browser, and when Claude on this Mac already holds the port the return is Claude's.
public struct McpSignInBrowser: Sendable {
    public init() {}

    public func open(_ url: URL, port: UInt16?) -> AsyncStream<McpSignInEvent> {
        AsyncStream { continuation in
            let task = Task { @MainActor in
                await Self.run(url, port: port, continuation)
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    @MainActor private static func run(_ url: URL, port: UInt16?, _ events: AsyncStream<McpSignInEvent>.Continuation) async {
        guard let port else {
            openOutside(url)
            return
        }
        let listener: LoopbackReturnListener?
        let route: McpReturnRoute
        do {
            listener = try await LoopbackReturnListener.open(port: port, page: returnPage)
            route = .listening
        } catch LoopbackReturnListener.Failure.portTaken {
            listener = nil
            #if os(macOS)
            route = .claudeOnThisMac
            #else
            route = .paste
            #endif
        } catch {
            listener = nil
            route = .paste
        }
        defer { listener?.close() }
        #if os(iOS)
        guard let listener else {
            openOutside(url)
            events.yield(.opened(route))
            return
        }
        let sheet = SignInSheet(url: url)
        guard sheet.start() else {
            openOutside(url)
            events.yield(.opened(.paste))
            return
        }
        events.yield(.opened(route))
        let returned = await withTaskCancellationHandler {
            await withTaskGroup(of: URL?.self) { group in
                group.addTask { await listener.next() }
                group.addTask { await sheet.closed(); return nil }
                let first = await group.next() ?? nil
                group.cancelAll()
                return first
            }
        } onCancel: {
            listener.close()
            Task { @MainActor in sheet.cancel() }
        }
        sheet.cancel()
        events.yield(returned.map(McpSignInEvent.returned) ?? .closed)
        #else
        openOutside(url)
        events.yield(.opened(route))
        guard let listener, let returned = await withTaskCancellationHandler(operation: { await listener.next() },
                                                                            onCancel: { listener.close() }) else { return }
        events.yield(.returned(returned))
        #endif
    }

    @MainActor private static func openOutside(_ url: URL) {
        #if os(iOS)
        UIApplication.shared.open(url)
        #else
        NSWorkspace.shared.open(url)
        #endif
    }

    private static let returnPage: @Sendable () -> String = {
        let title = String(localized: "Alondra has the sign-in")
        let body = String(localized: "You can close this page and go back to Alondra.")
        return "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">"
            + "<title>\(title)</title></head><body style=\"font-family:-apple-system;padding:2em\"><h1>\(title)</h1><p>\(body)</p></body></html>"
    }
}

#if os(iOS)
/// The system's sign-in sheet over the app. Its callback scheme never comes: the return is picked
/// up on localhost and the sheet is then closed by the app.
@MainActor private final class SignInSheet: NSObject, ASWebAuthenticationPresentationContextProviding {
    private var session: ASWebAuthenticationSession?
    private let window = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        .flatMap(\.windows).first(where: \.isKeyWindow)
    private var ended = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(url: URL) {
        super.init()
        session = ASWebAuthenticationSession(url: url, callback: .customScheme("alondra-mcp-sign-in")) { [weak self] _, _ in
            Task { @MainActor in self?.end() }
        }
        session?.presentationContextProvider = self
    }

    func start() -> Bool { window != nil && session?.start() == true }

    func cancel() {
        session?.cancel()
        end()
    }

    func closed() async {
        if ended { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    private func end() {
        ended = true
        let pending = waiters
        waiters = []
        pending.forEach { $0.resume() }
    }

    nonisolated func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        MainActor.assumeIsolated { window! }
    }
}
#endif
#endif
