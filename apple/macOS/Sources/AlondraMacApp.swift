import AlondraShared
import SwiftUI

@main
struct AlondraMacApp: App {
    var body: some Scene {
        WindowGroup("Alondra") {
            FleetView()
                .frame(minWidth: 860, minHeight: 540)
        }
        .defaultSize(width: 1240, height: 800)
        .commands {
            CommandGroup(replacing: .newItem) {}
            CommandGroup(after: .appSettings) {
                PairingCommand()
            }
        }

        Window("Vincular un iPhone", id: "pairing") {
            PairingView()
        }
        .windowResizability(.contentSize)
    }
}

struct PairingCommand: View {
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("Vincular un iPhone…") { openWindow(id: "pairing") }
            .keyboardShortcut("p", modifiers: [.command, .shift])
    }
}
