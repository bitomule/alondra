import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

struct DeliveryBanner: View {
    let store: StoreOf<MacAppFeature>
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

public struct MacNoticeToast: View {
    let store: StoreOf<MacAppFeature>
    public init(store: StoreOf<MacAppFeature>) { self.store = store }
    public var body: some View {
        ZStack {
            if let notice = store.notice {
                Label {
                    Text(verbatim: message(notice.event))
                } icon: {
                    Image(systemName: notice.failure ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                }
                .font(.subheadline.weight(.semibold))
                .lineLimit(2)
                .foregroundStyle(notice.failure ? Color.red : Color.primary)
                .padding(.horizontal, 16).padding(.vertical, 10)
                .glassEffect(.regular, in: .capsule)
                .padding(.bottom, 24)
                .transition(.move(edge: .bottom).combined(with: .opacity))
                .onTapGesture { store.send(.dismissNotice(notice.id)) }
                .accessibilityAction(named: "Dismiss") { store.send(.dismissNotice(notice.id)) }
                .id(notice.id)
            }
        }
        .animation(.snappy, value: store.notice?.id)
    }

    private func message(_ event: WorkspaceNotice.Event) -> String {
        switch event {
        case .actionCompleted(let message), .failure(let message): message
        }
    }
}

public struct UsageExhaustedWindowBanner: View {
    let usage: UsageExhausted?
    public init(usage: UsageExhausted?) { self.usage = usage }
    public var body: some View {
        ZStack {
            if let usage {
                UsageExhaustedBanner(usage)
                    .padding(.horizontal, 16).padding(.vertical, 10)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .animation(.snappy, value: usage)
    }
}
