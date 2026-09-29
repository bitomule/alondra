import AlondraShared
import SwiftUI

/// What stands between a PR and its merge, on its card and in its full screen.
struct PRStatus: View {
    let card: DomainPullRequestCard

    var body: some View {
        if let pr = card.observation { observed(pr) }
    }

    @ViewBuilder
    private func observed(_ pr: PullRequest) -> some View {
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

/// Open the PR on GitHub: the quiet line under its main button.
struct PRFooter: View {
    let card: DomainPullRequestCard

    var body: some View {
        HStack {
            if let url = card.webURL {
                Link(destination: url) {
                    Label("Open on GitHub", systemImage: "arrow.up.right.square")
                }
            }
            Spacer()
        }
        .font(.subheadline.weight(.medium))
        .foregroundStyle(.secondary)
    }
}
