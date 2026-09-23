import AlondraShared
import SwiftUI

@main
struct AlondraMacApp: App {
    var body: some Scene {
        WindowGroup("Alondra") {
            FleetView()
                .frame(minWidth: 900, minHeight: 560)
        }
    }
}
