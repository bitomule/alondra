import AlondraFeed
import Foundation

public extension ViewsClient {
    /// Network-free documents passed through the production reducers and renderer, in Spanish when
    /// the daemon has them in Spanish and `spanish` asks for it.
    static func demo(spanish: Bool = false) -> ViewsClient { DemoWorkspace.client(spanish: spanish) }

    /// The demo in this device's language.
    static func demoInDeviceLanguage() -> ViewsClient {
        demo(spanish: Locale.preferredLanguages.first?.hasPrefix("es") == true)
    }
}
