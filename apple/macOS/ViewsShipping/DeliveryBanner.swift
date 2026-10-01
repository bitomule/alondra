import AlondraFeatures
import ComposableArchitecture
import SwiftUI

struct DeliveryBanner: View {
    let store: StoreOf<AppFeature>
    var body: some View {
        ZStack {
            if let notice = store.macError, case .failure(let message) = notice.event {
                HStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text(message).lineLimit(2)
                    Button { store.send(.dismissMacError(notice.id)) } label: {
                        Image(systemName: "xmark").font(.caption.weight(.bold))
                    }
                    .buttonStyle(.borderless).accessibilityLabel("Close")
                }
                .padding(.horizontal, 14).padding(.vertical, 9)
                .background(.regularMaterial, in: .capsule)
                .shadow(color: .black.opacity(0.12), radius: 8, y: 2)
                .padding(.top, 10).transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .animation(.snappy, value: store.macError)
    }
}
