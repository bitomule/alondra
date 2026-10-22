import AlondraContract
import SwiftUI

struct MacCardButtonLabel: View {
    let action: AlondraContract.Action
    var body: some View {
        ZStack {
            VStack(spacing: 2) {
                Text(action.label).font(.subheadline.weight(.semibold)).multilineTextAlignment(.center)
                if let description = action.description {
                    Text(description).font(.caption).opacity(0.85).multilineTextAlignment(.center).lineLimit(2)
                }
            }
            .padding(.horizontal, 36)
            HStack {
                Spacer()
                if action.enabled, let key = action.key, key.count == 1 { MacKeyHint(key: key) }
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .frame(maxWidth: .infinity, minHeight: 34)
    }
}
