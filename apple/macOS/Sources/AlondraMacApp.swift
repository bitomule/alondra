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
        }
    }
}
