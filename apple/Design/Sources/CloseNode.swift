import SwiftUI

public struct CloseRequest: Identifiable, Hashable, Sendable {
    public let session: String
    public let name: String

    public var id: String { session }

    public init(session: String, name: String) {
        self.session = session
        self.name = name
    }

    var title: String { String(localized: "Close \(name)?", table: "AlondraDesign", bundle: #bundle) }

    var confirmTitle: String { String(localized: "Close Node", table: "AlondraDesign", bundle: #bundle) }
}

extension View {
    public func closeConfirmation(_ request: Binding<CloseRequest?>, confirm: @escaping (CloseRequest) -> Void) -> some View {
        alert(
            request.wrappedValue?.title ?? "",
            isPresented: Binding { request.wrappedValue != nil } set: { if !$0 { request.wrappedValue = nil } },
            presenting: request.wrappedValue
        ) { request in
            Button(request.confirmTitle, role: .destructive) { confirm(request) }
            Button(String(localized: "Cancel", table: "AlondraDesign", bundle: #bundle), role: .cancel) {}
        } message: { _ in
            Text(String(localized: "Runs claude rm. It never discards work: if the node has uncommitted or unpushed changes, or its worktree is still there, the server refuses and tells you why.", table: "AlondraDesign", bundle: #bundle))
        }
    }

    public func closeRefusalAlert(_ refusal: Binding<String?>) -> some View {
        alert(
            String(localized: "Couldn’t close the node", table: "AlondraDesign", bundle: #bundle),
            isPresented: Binding { refusal.wrappedValue != nil } set: { if !$0 { refusal.wrappedValue = nil } },
            presenting: refusal.wrappedValue
        ) { _ in
            Button(String(localized: "OK", table: "AlondraDesign", bundle: #bundle), role: .cancel) {}
        } message: { message in
            Text(verbatim: message)
        }
    }
}
