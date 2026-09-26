import AlondraShared
import AlondraPullRequestsFeature
import ComposableArchitecture
import SwiftUI

struct ServicePRMainAction: View {
    enum Placement: Equatable {
        case card
        case bar(prominent: Bool)
    }

    let store: StoreOf<PullRequestFeature>
    let placement: Placement
    let willSubmit: () -> Void
    init(store: StoreOf<PullRequestFeature>, placement: Placement = .card, willSubmit: @escaping () -> Void = {}) {
        self.store = store; self.placement = placement; self.willSubmit = willSubmit
    }

    var body: some View {
        if let pr = store.pr {
        switch pr.mainAction {
        case .resolveConflicts:
            Button {
                willSubmit()
                store.send(.resolveConflictsTapped)
            } label: {
                title(Label("Resolve Conflicts", systemImage: "arrow.triangle.branch"))
            }
            .modifier(Chrome(placement: placement, tint: Palette.dawn))
        case .merge(let methods, let blockedBy):
            Menu {
                ForEach(methods, id: \.self) { method in
                    Button(PullRequest.methodLabel(method)) { store.send(.chooseMergeMethod(method)) }
                }
            } label: {
                title(Label(blockedBy ?? String(localized: "Merge…"), systemImage: blockedBy == nil ? "arrow.triangle.merge" : "square.stack.3d.up"))
            }
            .modifier(Chrome(placement: placement, tint: blockedBy == nil ? Palette.sage : Palette.slate))
            .disabled(blockedBy != nil)
            .confirmationDialog("Merge PR #\(pr.number)?", isPresented: Binding(get: { store.selectedMergeMethod != nil }, set: { if !$0 { store.send(.cancelMerge) } }), titleVisibility: .visible) {
                if let method = store.selectedMergeMethod {
                    Button("Merge (\(PullRequest.methodLabel(method)))") {
                        willSubmit()
                        store.send(.confirmMerge)
                    }
                }
            } message: {
                Text(pr.summary)
            }
        }
        }
    }

    @ViewBuilder
    private func title(_ label: some View) -> some View {
        switch placement {
        case .card:
            label
                .font(.headline)
                .frame(maxWidth: .infinity, minHeight: 50)
        case .bar:
            label
                .frame(maxWidth: .infinity)
        }
    }

    private struct Chrome: ViewModifier {
        let placement: Placement
        let tint: Color

        func body(content: Content) -> some View {
            switch placement {
            case .card:
                content.buttonStyle(ServiceFilledStyle(tint: tint))
            case .bar(prominent: true):
                content.menuStyle(.button).buttonStyle(.glassProminent).tint(tint)
            case .bar(prominent: false):
                content.menuStyle(.button).buttonStyle(.glass).tint(tint)
            }
        }
    }
}

