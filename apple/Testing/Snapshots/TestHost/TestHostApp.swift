import SwiftUI

@main
struct TestHostApp: App {
    init() {
        UserDefaults.standard.set(["en-US"], forKey: "AppleLanguages")
        UserDefaults.standard.set("en_US", forKey: "AppleLocale")
    }

    var body: some Scene {
        WindowGroup {
            Color.clear
        }
    }
}
