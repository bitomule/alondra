import SwiftUI

public struct CloseRequest: Identifiable, Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case node
        case readAndClose
    }

    public let session: String
    public let name: String
    public let kind: Kind

    public var id: String { session }

    public init(session: String, name: String, kind: Kind) {
        self.session = session
        self.name = name
        self.kind = kind
    }

    var title: String {
        switch kind {
        case .node: String(localized: "Close \(name)?", table: "AlondraShared", bundle: #bundle)
        case .readAndClose: String(localized: "Mark as read and close \(name)?", table: "AlondraShared", bundle: #bundle)
        }
    }

    var confirmTitle: String {
        switch kind {
        case .node: String(localized: "Close Node", table: "AlondraShared", bundle: #bundle)
        case .readAndClose: String(localized: "Read and Close Node", table: "AlondraShared", bundle: #bundle)
        }
    }
}

extension View {
    public func closeConfirmation(_ request: Binding<CloseRequest?>, confirm: @escaping (CloseRequest) -> Void) -> some View {
        confirmationDialog(
            request.wrappedValue?.title ?? "",
            isPresented: Binding { request.wrappedValue != nil } set: { if !$0 { request.wrappedValue = nil } },
            titleVisibility: .visible,
            presenting: request.wrappedValue
        ) { request in
            Button(request.confirmTitle, role: .destructive) { confirm(request) }
            Button(String(localized: "Cancel", table: "AlondraShared", bundle: #bundle), role: .cancel) {}
        } message: { _ in
            Text(String(localized: "Runs claude rm. It never discards work: if the node has uncommitted or unpushed changes, or its worktree is still there, the server refuses and tells you why.", table: "AlondraShared", bundle: #bundle))
        }
    }

    public func closeRefusalAlert(_ store: FleetStore) -> some View {
        alert(
            String(localized: "Couldn’t close the node", table: "AlondraShared", bundle: #bundle),
            isPresented: Binding { store.refusal != nil } set: { if !$0 { store.refusal = nil } },
            presenting: store.refusal
        ) { _ in
            Button(String(localized: "OK", table: "AlondraShared", bundle: #bundle), role: .cancel) {}
        } message: { message in
            Text(verbatim: message)
        }
    }
}
