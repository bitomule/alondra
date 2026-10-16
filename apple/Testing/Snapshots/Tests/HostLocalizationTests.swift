import Foundation
import XCTest

/// The Spanish captures read their strings from the test host. A simulator that kept an earlier
/// build's copy of the host draws every newer key in English, so the host must carry the catalog's
/// current translations.
final class HostLocalizationTests: XCTestCase {
    private struct Catalog: Decodable {
        struct Entry: Decodable {
            struct Localization: Decodable {
                struct StringUnit: Decodable { let value: String }
                let stringUnit: StringUnit?
            }
            let localizations: [String: Localization]?
        }
        let strings: [String: Entry]
    }

    func test_theHostCarriesEverySpanishTranslationInTheCatalog() throws {
        let root = try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"])
        let url = URL(fileURLWithPath: root).appendingPathComponent("_main/apple/iOS/Resources/Localizable.xcstrings")
        let catalog = try JSONDecoder().decode(Catalog.self, from: Data(contentsOf: url))
        let spanish = try XCTUnwrap(Bundle.main.path(forResource: "es", ofType: "lproj").flatMap(Bundle.init(path:)))
        let stale = catalog.strings.compactMap { key, entry -> String? in
            guard let value = entry.localizations?["es"]?.stringUnit?.value else { return nil }
            return spanish.localizedString(forKey: key, value: nil, table: nil) == value ? nil : key
        }
        XCTAssertEqual(stale.sorted(), [], "\(Bundle.main.bundlePath) does not have these Spanish strings")
    }
}
