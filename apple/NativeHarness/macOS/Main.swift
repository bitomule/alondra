import AlondraNativeScenarios
import Foundation

@main
struct HarnessMain {
    static func main() async {
        let code = await NativeScenarioRunner.run(environment: ProcessInfo.processInfo.environment)
        exit(code)
    }
}
