import AlondraDesign
import ComposableArchitecture
import SwiftUI

/// Settings' «Send a crash report» row: it opens the report's screen.
public struct CrashReportRow: View {
    let store: StoreOf<CrashReportFeature>

    public init(store: StoreOf<CrashReportFeature>) { self.store = store }

    public var body: some View {
        Section {
            NavigationLink {
                CrashReportScreen(store: store)
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Send a crash report").foregroundStyle(Palette.ink)
                    Text("Logs of your Mac with everything private left out. You see all of it before it is sent.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
            }
            .accessibilityIdentifier("settings.crashReport")
        }
    }
}
