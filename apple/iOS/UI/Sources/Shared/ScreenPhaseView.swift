import AlondraDesign
import AlondraFeatures
import SwiftUI

/// What a screen shows until its view has arrived: the spinner, the daemon no longer serving the
/// view, or a failed load with a way to try again. While the Mac is not answering it says why, in
/// the words the rest of the app uses.
struct ScreenPhaseView: View {
    let phase: ScreenFeature.State.Phase
    let missing: LocalizedStringKey
    let retry: () -> Void
    @Environment(\.connectionState) private var connection

    var body: some View {
        if case .lost(let problem) = ConnectionIndicators(connection, hasSavedData: false).cover, phase != .missing {
            ConnectionUnavailable(problem: problem, retry: retry)
                .tint(Palette.dawn)
                .padding(.top, 60)
        } else {
            content
        }
    }

    @ViewBuilder private var content: some View {
        switch phase {
        case .loading, .loaded:
            ProgressView()
                .controlSize(.large)
                .tint(Palette.dawn)
                .frame(maxWidth: .infinity)
                .padding(.top, 120)
        case .missing:
            ContentUnavailableView(missing, systemImage: "questionmark.circle")
                .padding(.top, 60)
        case .failed(let message):
            ContentUnavailableView {
                Label("Couldn’t load this", systemImage: "exclamationmark.triangle")
            } description: {
                Text(verbatim: message)
            } actions: {
                Button("Try Again", action: retry)
                    .buttonStyle(.borderedProminent)
                    .tint(Palette.dawn)
            }
            .padding(.top, 60)
        }
    }
}
