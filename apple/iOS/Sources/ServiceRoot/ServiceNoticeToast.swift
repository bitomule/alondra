import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI

/// Original notice presentation; expiration and operation ownership live in the reducer.
struct ServiceNoticeToast: View {
    let store: StoreOf<ServiceRootFeature>
    var body: some View {
        VStack(spacing: 8) {
            if let notice = store.notice {
                Label {
                    noticeText(notice.event)
                } icon: {
                    Image(systemName: notice.failure ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                }
                .font(.subheadline.weight(.semibold))
                .lineLimit(2)
                .truncationMode(.tail)
                .foregroundStyle(notice.failure ? Palette.rose : Palette.ink)
                .padding(.horizontal, 18)
                .padding(.vertical, 12)
                .glassEffect(.regular, in: .rect(cornerRadius: 22))
                .padding(.horizontal, 24)
                .transition(.move(edge: .bottom).combined(with: .opacity).combined(with: .scale(scale: 0.9)))
                .onTapGesture { store.send(.dismissNotice(notice.id)) }
                .accessibilityAction(named: "Dismiss") { store.send(.dismissNotice(notice.id)) }
                .id(notice.id)
            }
        }
        .padding(.bottom, 92)
        .animation(.spring(duration: 0.45, bounce: 0.3), value: store.notice)
        .sensoryFeedback(trigger: store.notice) { _, new in
            new?.failure == true ? .error : nil
        }
    }

    @ViewBuilder private func noticeText(_ event: WorkspaceNotice.Event) -> some View {
        switch event {
        case .sentToOrchestrator: Text("Sent to the orchestrator")
        case .messageSent(let name): Text("Message sent to \(name)")
        case .closed(let name): Text("\(name) closed")
        case .failure(let message), .closeRefused(let message): Text(verbatim: message)
        }
    }
}
