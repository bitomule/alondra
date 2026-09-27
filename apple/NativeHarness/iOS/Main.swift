import AlondraNativeScenarios
import UIKit

@main
final class HarnessAppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let environment = ProcessInfo.processInfo.environment
        Task {
            let code = await NativeScenarioRunner.run(environment: environment)
            exit(code)
        }
        return true
    }
}
