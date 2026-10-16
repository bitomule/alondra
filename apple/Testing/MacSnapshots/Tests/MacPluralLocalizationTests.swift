import Foundation
import XCTest

/// Every count the Mac's catalogs vary by plural reads in Spanish in the strings the Spanish captures draw with,
/// so a capture can't show "512 requests" beside Spanish text again.
final class MacPluralLocalizationTests: XCTestCase {
    private let catalogs = ["apple/macOS/Resources/Localizable.xcstrings", "apple/Design/Resources/AlondraDesign.xcstrings"]
    private let spanish = Locale(identifier: "es_ES")

    func test_everyPluralReadsInSpanish() throws {
        let strings = try XCTUnwrap(Bundle.main.path(forResource: "es", ofType: "lproj").flatMap(Bundle.init(path:)))
        let plurals = try spanishPlurals()
        XCTAssertFalse(plurals.isEmpty)
        for (key, forms) in plurals {
            let format = strings.localizedString(forKey: key, value: nil, table: nil)
            for (count, form) in [(1, "one"), (2, "other")] {
                let expected = try XCTUnwrap(forms[form], "\(key) has no Spanish \(form) form")
                XCTAssertEqual(String(format: format, locale: spanish, count), String(format: expected, locale: spanish, count),
                               "\(key) with \(count)")
            }
        }
    }

    private func spanishPlurals() throws -> [String: [String: String]] {
        let environment = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
        var plurals: [String: [String: String]] = [:]
        for catalog in catalogs {
            let catalog = try JSONDecoder().decode(Catalog.self, from: Data(contentsOf: root.appendingPathComponent(catalog)))
            for (key, entry) in catalog.strings {
                guard let forms = entry.localizations?["es"]?.variations?.plural else { continue }
                plurals[key] = forms.mapValues(\.stringUnit.value)
            }
        }
        return plurals
    }
}

private struct Catalog: Decodable {
    struct Entry: Decodable { var localizations: [String: Localization]? }
    struct Localization: Decodable { var variations: Variations? }
    struct Variations: Decodable { var plural: [String: Form]? }
    struct Form: Decodable { var stringUnit: Unit }
    struct Unit: Decodable { var value: String }

    var strings: [String: Entry]
}
