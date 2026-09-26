import AlondraAttachmentsFeature
import ComposableArchitecture
import SwiftUI
import UniformTypeIdentifiers

struct ServiceAttachFilesButton: View {
    let store: StoreOf<AttachmentFeature>

    var body: some View {
        Button {
            store.send(.showFiles(true))
        } label: {
            Label("Attach Files…", systemImage: "paperclip")
                .labelStyle(.iconOnly)
        }
        .buttonStyle(.borderless)
        .controlSize(.large)
        .help(Text("Attach files (or drop them here)"))
        .fileImporter(isPresented: Binding(get: { store.filesPresented }, set: { store.send(.showFiles($0)) }), allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            guard case .success(let urls) = result else { return }
            store.send(.filesSelected(urls))
        }
        .alert("Couldn’t Attach", isPresented: .init(get: { store.importError != nil }, set: { if !$0 { store.send(.dismissError) } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(store.importError ?? "")
        }
    }
}

/// Files dropped on a text box are attached to its message.
struct ServiceDropsFiles: ViewModifier {
    let store: StoreOf<AttachmentFeature>?

    func body(content: Content) -> some View {
        if let store {
            content
                .overlay {
                    if store.dropTargeted {
                        RoundedRectangle(cornerRadius: 12)
                            .strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 2, dash: [6, 4]))
                            .background(Color.accentColor.opacity(0.08), in: .rect(cornerRadius: 12))
                            .allowsHitTesting(false)
                    }
                }
                .dropDestination(for: URL.self) { urls, _ in
                    store.send(.filesSelected(urls))
                    return !urls.isEmpty
                } isTargeted: { store.send(.dropTargetChanged($0)) }
                .alert("Couldn’t Attach", isPresented: .init(get: { store.importError != nil }, set: { if !$0 { store.send(.dismissError) } })) {
                    Button("OK", role: .cancel) {}
                } message: {
                    Text(store.importError ?? "")
                }
        } else {
            content
        }
    }
}
