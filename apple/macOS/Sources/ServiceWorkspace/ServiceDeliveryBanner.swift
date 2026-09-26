import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI

struct ServiceDeliveryBanner: View {
    let store: StoreOf<ServiceRootFeature>
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
            } else if !store.delivery.warnings.isEmpty {
                HStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text(store.delivery.warningText).lineLimit(2)
                    Button { store.send(.delivery(.dismissWarnings)) } label: {
                        Image(systemName: "xmark").font(.caption.weight(.bold))
                    }
                    .disabled(store.delivery.isAcknowledging)
                    .buttonStyle(.borderless).accessibilityLabel("Close")
                }
                .padding(.horizontal, 14).padding(.vertical, 9)
                .background(.regularMaterial, in: .capsule).padding(.top, 10).transition(.opacity)
            } else if store.delivery.pendingCount > 0 {
                Label(String(localized: "\(store.delivery.pendingCount) operations unconfirmed; checking their status"), systemImage: "arrow.clockwise")
                    .lineLimit(1).foregroundStyle(.secondary)
                    .padding(.horizontal, 14).padding(.vertical, 9)
                    .background(.regularMaterial, in: .capsule).padding(.top, 10).transition(.opacity)
            }
        }
        .animation(.snappy, value: store.macError)
        .animation(.snappy, value: store.delivery.pendingCount)
        .animation(.snappy, value: store.delivery.warnings)
    }
}
