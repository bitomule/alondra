import AlondraWorkspaceFeature
import ComposableArchitecture
import Observation
import SwiftUI

@Reducer public struct DemoModeFeature {
    @ObservableState public struct State: Equatable {
        public var sessionID: UUID?
        public init() {}
    }
    public enum Action { case enter, reset, exit }
    public init() {}
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .enter, .reset: state.sessionID = UUID()
            case .exit: state.sessionID = nil
            }
            return .none
        }
    }
}

public struct DemoEntryButton: View {
    let store: StoreOf<DemoModeFeature>
    public init(store: StoreOf<DemoModeFeature>) { self.store = store }
    public var body: some View {
        Button("Explore the demo") { store.send(.enter) }
            .accessibilityIdentifier("demo.enter")
    }
}

/// Existing app tabs/sidebar and feature views are injected as content. The demo
/// never receives a CloudPairingOwner or any production service dependency.
public struct DemoWorkspaceScreen<Content: View>: View {
    let mode: StoreOf<DemoModeFeature>
    let content: (StoreOf<ServiceRootFeature>) -> Content
    @State private var context = DemoPresentationContext()
    public init(mode: StoreOf<DemoModeFeature>, @ViewBuilder content: @escaping (StoreOf<ServiceRootFeature>) -> Content) {
        self.mode = mode; self.content = content
    }
    public var body: some View {
        VStack(spacing: 0) {
            HStack {
                Label("Demo · Sample data", systemImage: "sparkles")
                Spacer()
                Button("Reset demo") { mode.send(.reset) }.accessibilityIdentifier("demo.reset")
                Button("Exit demo") { mode.send(.exit) }.accessibilityIdentifier("demo.exit")
            }.font(.callout).padding().background(.thinMaterial)
            if let store = context.store { content(store) }
            else if let error = context.error { Text(error).padding() }
            else { ProgressView("Opening demo…").frame(maxWidth: .infinity, maxHeight: .infinity) }
        }
        .environment(\.openURL, OpenURLAction { _ in .discarded })
        .task(id: mode.sessionID) { await context.replace(mode.sessionID) }
        .onDisappear { context.close() }
    }
}

public struct DemoAccountView: View {
    public init() {}
    public var body: some View {
        ContentUnavailableView("Demo workspace", systemImage: "sparkles",
            description: Text("Everything here is sample data. Actions stay in memory and never reach a real account. Use Reset demo to start over or Exit demo to connect your workspace."))
    }
}

@MainActor @Observable private final class DemoPresentationContext {
    private(set) var store: StoreOf<ServiceRootFeature>?
    private(set) var error: String?
    private var session: DemoWorkspace?
    private var generation = 0
    func replace(_ id: UUID?) async {
        generation += 1
        let current = generation
        let previous = session
        session = nil; store = nil; error = nil
        await previous?.close()
        guard id != nil, current == generation, !Task.isCancelled else { return }
        do {
            let session = try DemoWorkspace()
            let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
                session.install(into: &$0)
            }
            self.session = session; self.store = store
        } catch { self.error = error.localizedDescription }
    }
    func close() {
        generation += 1
        let old = session
        session = nil; store = nil
        Task { await old?.close() }
    }
}
