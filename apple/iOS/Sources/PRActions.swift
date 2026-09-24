import AlondraShared
import SwiftUI

/// What stands between a PR and its merge, on its card and in its full screen.
struct PRStatus: View {
    let pr: PullRequest

    var body: some View {
        if pr.hasConflicts {
            Label("It has conflicts with its base branch", systemImage: "exclamationmark.triangle.fill")
                .font(.footnote.weight(.medium))
                .foregroundStyle(Palette.dawn)
        } else if pr.mergeabilityUnknown {
            Label("GitHub is checking whether it can be merged", systemImage: "clock")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
        if let stackedOn = pr.stackedOn {
            Label(stackedOn, systemImage: "square.stack.3d.up")
                .font(.footnote.weight(.medium))
                .foregroundStyle(.secondary)
        }
        ForEach(pr.checks ?? [], id: \.self) { check in
            Label(check.name, systemImage: check.passed ? "checkmark.circle.fill" : check.failed ? "xmark.circle.fill" : "clock")
                .font(.footnote)
                .foregroundStyle(check.passed ? Palette.sage : check.failed ? Palette.rose : Palette.dawn)
        }
    }
}

/// The PR's main button: send the conflicts to its node, or pick a merge method and confirm it.
struct PRMainAction: View {
    enum Placement: Equatable {
        case card
        case bar(prominent: Bool)
    }

    let pr: PullRequest
    let placement: Placement
    let onOutcome: (PullRequestOutcome) -> Void
    @State private var merging: String?

    var body: some View {
        switch pr.mainAction {
        case .resolveConflicts:
            Button {
                onOutcome(.conflictsSent)
            } label: {
                title(Label("Resolve Conflicts", systemImage: "arrow.triangle.branch"))
            }
            .modifier(Chrome(placement: placement, tint: Palette.dawn))
        case .merge(let methods, let blockedBy):
            Menu {
                ForEach(methods, id: \.self) { method in
                    Button(PullRequest.methodLabel(method)) { merging = method }
                }
            } label: {
                title(Label(blockedBy ?? String(localized: "Merge…"), systemImage: blockedBy == nil ? "arrow.triangle.merge" : "square.stack.3d.up"))
            }
            .modifier(Chrome(placement: placement, tint: blockedBy == nil ? Palette.sage : Palette.slate))
            .disabled(blockedBy != nil)
            .confirmationDialog("Merge PR #\(pr.number)?", isPresented: Binding(get: { merging != nil }, set: { if !$0 { merging = nil } }), titleVisibility: .visible) {
                if let method = merging {
                    Button("Merge (\(PullRequest.methodLabel(method)))") {
                        onOutcome(.merged(method: method))
                    }
                }
            } message: {
                Text(pr.summary)
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
                content.buttonStyle(FilledStyle(tint: tint))
            case .bar(prominent: true):
                content.menuStyle(.button).buttonStyle(.glassProminent).tint(tint)
            case .bar(prominent: false):
                content.menuStyle(.button).buttonStyle(.glass).tint(tint)
            }
        }
    }
}

/// Open the PR on GitHub, or set it aside: the quiet line under its main button.
struct PRFooter: View {
    let pr: PullRequest
    let onSetAside: () -> Void

    var body: some View {
        HStack {
            if let url = URL(string: pr.url) {
                Link(destination: url) {
                    Label("Open on GitHub", systemImage: "arrow.up.right.square")
                }
            }
            Spacer()
            Button("Set Aside", action: onSetAside)
        }
        .font(.subheadline.weight(.medium))
        .foregroundStyle(.secondary)
    }
}

extension PullRequestOutcome {
    var handoffLabel: String {
        switch self {
        case .merged: HandoffLabel.merged
        case .conflictsSent: HandoffLabel.conflictsSent
        case .setAside: HandoffLabel.setAside
        }
    }
}
