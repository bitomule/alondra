#if os(iOS) || os(macOS)
import AuthenticationServices
import Foundation

@MainActor
public final class SystemAuthBrowser: NSObject, AuthBrowser {
    private let anchor: @MainActor () -> ASPresentationAnchor?
    private let gate = BrowserSessionGate()

    public init(anchor: @escaping @MainActor () -> ASPresentationAnchor?) { self.anchor = anchor }

    public func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL {
        guard !gate.isSuperseded(generation) else { throw CancellationError() }
        guard !gate.isBusy else { throw AuthError.invalidRedirect }
        guard let window = anchor() else { throw AuthError.invalidRedirect }
        let context = PresentationContext(window: window)
        let id = UUID()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                if Task.isCancelled || gate.isSuperseded(generation) {
                    continuation.resume(throwing: CancellationError())
                    return
                }
                let session = ASWebAuthenticationSession(url: url, callbackURLScheme: callbackScheme) { [weak self, context] callback, error in
                    withExtendedLifetime(context) {
                        self?.gate.finish(id: id, callback: callback, error: error)
                    }
                }
                session.presentationContextProvider = context
                guard gate.begin(id: id, generation: generation, cancel: { session.cancel() }, continuation: continuation) else {
                    return
                }
                guard session.start() else {
                    gate.finish(id: id, callback: nil, error: AuthError.invalidRedirect)
                    return
                }
            }
        } onCancel: {
            // Cancellation may hop to MainActor after a new browser session starts.
            // The ID prevents A's delayed handler from cancelling B.
            Task { @MainActor [weak self] in self?.gate.cancel(id: id) }
        }
    }

    public func cancelAuthentication(generation: Int) async {
        gate.invalidate(generation: generation)
    }

    private final class PresentationContext: NSObject, ASWebAuthenticationPresentationContextProviding {
        let window: ASPresentationAnchor
        init(window: ASPresentationAnchor) { self.window = window }
        func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor { window }
    }
}
#endif
