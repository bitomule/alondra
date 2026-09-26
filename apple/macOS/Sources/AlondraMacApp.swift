import AlondraShared
import SwiftUI

@main
struct AlondraMacApp: App {
    @State private var device: LocalDevice
    @State private var sync: MacAppSync

    init() {
        let device = LocalDevice()
        let sync = MacAppSync(client: device.client)
        device.onClientChanged = { [sync] client in
            let scope = sync.prepareUse(client)
            Task { await sync.activate(client, scope: scope) }
        }
        _device = State(initialValue: device)
        _sync = State(initialValue: sync)
    }

    var body: some Scene {
        WindowGroup("Alondra", id: "main") {
            MacWindowScene(device: device, services: sync)
        }
        .defaultSize(width: 1240, height: 800)
        .commands {
            CommandGroup(after: .appSettings) {
                PairingCommand()
            }
        }

        Window("Pair an iPhone", id: "pairing") {
            PairingView()
                .id(MacAppSync.identity(for: device.client))
                .environment(device)
                .environment(sync)
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
