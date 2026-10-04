import SwiftUI

struct SidebarCountHeader: View {
    let title: String
    let count: Int
    let tint: Color

    var body: some View {
        HStack(spacing: 6) {
            Text(title)
            if count > 0 {
                Text(count, format: .number)
                    .monospacedDigit()
                    .foregroundStyle(.white)
                    .padding(.horizontal, 6)
                    .background(tint, in: .capsule)
            }
        }
    }
}
