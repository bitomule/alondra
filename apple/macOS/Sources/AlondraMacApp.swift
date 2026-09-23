import AlondraShared
import SwiftUI

@main
struct AlondraMacApp: App {
    @State private var device = LocalDevice()

    var body: some Scene {
        WindowGroup("Alondra") {
            FleetView()
                .frame(minWidth: 860, minHeight: 540)
                .environment(device)
        }
        .defaultSize(width: 1240, height: 800)
        .commands {
            CommandGroup(replacing: .newItem) {}
            CommandGroup(after: .appSettings) {
                PairingCommand()
            }
        }

        Window("Pair an iPhone", id: "pairing") {
            PairingView()
                .environment(device)
        }
        .windowResizability(.contentSize)
    }
}

struct PairingCommand: View {
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("Pair an iPhone…") { openWindow(id: "pairing") }
            .keyboardShortcut("p", modifiers: [.command, .shift])
    }
}
