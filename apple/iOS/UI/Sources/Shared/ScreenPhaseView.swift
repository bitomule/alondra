import AlondraDesign
import AlondraFeatures
import SwiftUI

/// What a screen shows until its view has arrived: the spinner, the daemon no longer serving the
/// view, or a failed load with a way to try again.
struct ScreenPhaseView: View {
    let phase: ScreenFeature.State.Phase
    let missing: LocalizedStringKey
    let retry: () -> Void

    var body: some View {
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
