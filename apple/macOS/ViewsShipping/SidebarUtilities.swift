import SwiftUI

func shortName(_ name: String?) -> String {
    guard let name, !name.isEmpty else { return String(localized: "unnamed") }
    return name.split(separator: "/").last.map(String.init) ?? name
}

struct StatusDot: View {
    let color: Color
    var size: CGFloat = 8

    var body: some View {
        Circle().fill(color).frame(width: size, height: size).accessibilityHidden(true)
    }
}
