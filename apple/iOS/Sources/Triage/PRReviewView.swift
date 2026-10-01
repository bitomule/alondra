import AlondraDesign
import AlondraContract
import AlondraFeatures
import SwiftUI

struct PRReviewView: View {
    let document: ViewDocument
    let diff: String?
    @Binding var comments: [String: ActionComment]
    let onAction: (AlondraContract.Action, String?) -> Void
    let onLink: (AlondraContract.Link) -> Void
    let onContent: (ContentRef) -> Void
    let onAttachment: (AlondraContract.Attachment) -> Void

    private var actions: [AlondraContract.Action] {
        document.sections.flatMap(\.items).flatMap { item -> [AlondraContract.Action] in
            if case let .actions(bar) = item { return bar.actions }
            return []
        }.filter { $0.enabled || $0.id?.hasPrefix("accept:") != true }
    }
    private var obsoleteAcceptIDs: Set<String> {
        Set(document.sections.flatMap(\.items).flatMap { item -> [AlondraContract.Action] in
            if case let .card(card) = item {
                return card.actions + card.children.flatMap(\.actions)
            }
            return []
        }.compactMap { action in
            guard !action.enabled, action.id?.hasPrefix("accept:") == true else { return nil }
            return action.id
        })
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(document.title ?? String(localized: "Pull Request"))
                    .font(.title3.weight(.bold)).foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                ForEach(document.notices, id: \.text) { NoticeView(notice: $0) }
                ForEach(document.sections.filter { $0.id == "description" }, id: \.id) { section in
                    SectionView(section: section, onAction: { onAction($0, nil) }, onLink: onLink,
                                onContent: onContent, onAttachment: onAttachment,
                                excludingActionIDs: obsoleteAcceptIDs)
                }
                ForEach(document.sections.filter { !["description", "actions", "diff"].contains($0.id) }, id: \.id) { section in
                    SectionView(section: section, onAction: { onAction($0, nil) }, onLink: onLink,
                                onContent: onContent, onAttachment: onAttachment,
                                excludingActionIDs: obsoleteAcceptIDs)
                }
                if let diff {
                    DiffView(text: diff, comments: $comments)
                }
            }
            .padding(16)
        }
        .background(DawnBackground(glow: Palette.dawn))
        .navigationTitle(document.title ?? String(localized: "Pull Request"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if !actions.isEmpty {
                actionBar
            }
        }
    }

    private var actionBar: some View {
        VStack(spacing: 8) {
            ForEach(actions, id: \.label) { action in
                if action.presentation == .menu, !action.options.isEmpty {
                    Menu {
                        ForEach(action.options, id: \.id) { option in
                            Button(option.label) { onAction(action, option.id) }
                        }
                    } label: {
                        Label(action.label + "…", systemImage: "arrow.triangle.merge")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.glassProminent).tint(Palette.sageDeep)
                    .disabled(!action.enabled)
                } else if let link = action.link, action.id == nil {
                    Button { onLink(link) } label: {
                        Label(action.label, systemImage: "arrow.up.right.square")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.glass)
                } else {
                    Button { onAction(action, nil) } label: {
                        if action.id?.hasPrefix("changes:") == true {
                            Label(action.label, systemImage: "arrow.uturn.backward")
                                .frame(maxWidth: .infinity)
                        } else if action.id?.hasPrefix("ask_pr:") == true {
                            Label(action.label, systemImage: "questionmark.bubble")
                                .frame(maxWidth: .infinity)
                        } else {
                            Text(action.label).frame(maxWidth: .infinity)
                        }
                    }
                    .buttonStyle(.glass)
                    .disabled(!action.enabled)
                }
            }
        }
        .controlSize(.large)
        .buttonBorderShape(.capsule)
        .font(.headline)
        .padding(.horizontal, 16).padding(.vertical, 10)
        .background(.bar)
    }
}
