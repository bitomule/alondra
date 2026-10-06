import SnapshotTesting
import SwiftUI
import UIKit

/// Every baseline is a function of the code and nothing else: locale, calendar, time zone and the
/// instant relative dates are measured against are pinned here, not read from the simulator.
let snapshotReferenceDate = Date(timeIntervalSince1970: 1_767_225_600)

private let snapshotTimeZone = TimeZone(identifier: "UTC")!

enum SnapshotLanguage: String {
    case english = "en_US"
    case spanish = "es_ES"

    var locale: Locale { Locale(identifier: rawValue) }

    var calendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = locale
        calendar.timeZone = snapshotTimeZone
        return calendar
    }
}

/// How a screen is captured: one name per variation, so a baseline file reads
/// `test_<state>_<light|dark|ax3>[_es]`.
enum SnapshotVariant: CaseIterable {
    case light
    case dark
    case ax3

    var suffix: String {
        switch self {
        case .light: "light"
        case .dark: "dark"
        case .ax3: "ax3"
        }
    }

    var style: UIUserInterfaceStyle { self == .dark ? .dark : .light }

    var dynamicType: DynamicTypeSize { self == .ax3 ? .accessibility3 : .large }
}

/// A view laid out at the width of an iPhone 17 Pro, as tall as its content, rendered in the pinned
/// environment.
@MainActor
func screen<Content: View>(
    _ variant: SnapshotVariant = .light,
    language: SnapshotLanguage = .english,
    width: CGFloat = 393,
    @ViewBuilder _ content: () -> Content
) -> AnyView {
    AnyView(
        content()
            .frame(width: width)
            .environment(\.locale, language.locale)
            .environment(\.calendar, language.calendar)
            .environment(\.timeZone, snapshotTimeZone)
            .dynamicTypeSize(variant.dynamicType)
    )
}

/// perceptualPrecision 0.98 tolerates Apple Silicon anti-aliasing without hiding a real change.
func deviceImage(_ variant: SnapshotVariant) -> Snapshotting<AnyView, UIImage> {
    Snapshotting<AnyView, UIImage>.image(
        perceptualPrecision: 0.98,
        layout: .sizeThatFits,
        traits: .init(userInterfaceStyle: variant.style)
    ).asRecorded
}

extension Snapshotting where Format == UIImage {
    /// A capture is extended sRGB in 16-bit floats and a reference is the Display P3 PNG it was
    /// recorded as, and the perceptual comparison reads both without colour management. Comparing
    /// the capture as the PNG it would be recorded as keeps a one-step difference in a shadow from
    /// failing every saturated pixel on the screen.
    var asRecorded: Self {
        Snapshotting(pathExtension: pathExtension, diffing: diffing) { value in
            snapshot(value).map { image in
                image.pngData().flatMap { UIImage(data: $0, scale: image.scale) } ?? image
            }
        }
    }
}

/// Captures one view in light, dark and accessibility size 3. The baselines are named after the
/// calling test and the variant: `test_<state>.light.png`, `.dark.png`, `.ax3.png` (and `_es` for
/// Spanish).
@MainActor
func assertVariants<Content: View>(
    language: SnapshotLanguage = .english,
    file: StaticString = #filePath,
    testName: String = #function,
    line: UInt = #line,
    @ViewBuilder _ content: () -> Content
) {
    for variant in SnapshotVariant.allCases {
        let suffix = language == .english ? variant.suffix : "\(variant.suffix)_\(language.rawValue.prefix(2))"
        assertAppSnapshot(
            of: screen(variant, language: language, content),
            as: deviceImage(variant),
            named: suffix,
            file: file,
            testName: testName,
            line: line
        )
    }
}
