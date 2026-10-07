import Foundation
import ObjectiveC

/// The Mac app's Spanish strings, for a test process that has no app bundle. The catalog in the
/// runfiles becomes `es.lproj/Localizable.strings` in a temporary bundle, and that bundle answers
/// as the main one, where SwiftUI looks its strings up.
enum MacCatalogBundle {
    @MainActor private static var installed = false

    @MainActor static func installSpanish() throws {
        guard !installed else { return }
        let environment = ProcessInfo.processInfo.environment
        guard let runfiles = environment["TEST_SRCDIR"] else { throw CocoaError(.fileNoSuchFile) }
        let catalog = URL(fileURLWithPath: runfiles)
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("apple/macOS/Resources/Localizable.xcstrings")
        let json = try JSONSerialization.jsonObject(with: Data(contentsOf: catalog)) as? [String: Any]
        let strings = json?["strings"] as? [String: Any] ?? [:]
        var spanish: [String: String] = [:]
        for (key, entry) in strings {
            let localizations = (entry as? [String: Any])?["localizations"] as? [String: Any]
            let unit = (localizations?["es"] as? [String: Any])?["stringUnit"] as? [String: Any]
            if let value = unit?["value"] as? String { spanish[key] = value }
        }
        let bundle = FileManager.default.temporaryDirectory
            .appendingPathComponent("MacCatalog-\(UUID().uuidString).bundle")
        let lproj = bundle.appendingPathComponent("es.lproj")
        try FileManager.default.createDirectory(at: lproj, withIntermediateDirectories: true)
        try PropertyListSerialization.data(fromPropertyList: spanish, format: .xml, options: 0)
            .write(to: lproj.appendingPathComponent("Localizable.strings"))
        guard let loaded = Bundle(url: bundle),
              let method = class_getClassMethod(Bundle.self, #selector(getter: Bundle.main)) else {
            throw CocoaError(.fileReadCorruptFile)
        }
        let answer: @convention(block) (AnyObject) -> Bundle = { _ in loaded }
        method_setImplementation(method, imp_implementationWithBlock(answer))
        installed = true
    }
}
