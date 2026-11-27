import SwiftUI

/// A few small items side by side, or one under another when `stacked`. The caller decides from
/// what the items say, so the height never depends on a width measured twice.
struct ChipRow<Content: View>: View {
    var stacked = false
    @ViewBuilder let content: Content

    var body: some View {
        if stacked {
            VStack(alignment: .leading, spacing: 6) { content }
        } else {
            HStack(spacing: 8) { content }
        }
    }
}
