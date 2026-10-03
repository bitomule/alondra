#if os(iOS)
import AlondraAttachments
import ComposableArchitecture
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// The attachment menu, with presentation and import owned by TCA: a bare paperclip inside a
/// composer's glass, or a titled glass capsule where it stands on its own.
public struct AttachButton: View {
    public let store: StoreOf<AttachmentFeature>
    public let title: Text?

    public init(store: StoreOf<AttachmentFeature>, title: Text? = nil) {
        self.store = store
        self.title = title
    }

    public var body: some View {
        Group {
            if let title {
                menu { Label { title } icon: { Image(systemName: "paperclip") } }
                    .menuStyle(.button)
                    .buttonStyle(.glass)
                    .buttonBorderShape(.capsule)
            } else {
                menu {
                    Image(systemName: "paperclip")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .frame(width: 36, height: 36)
                }
                .accessibilityLabel("Attach")
            }
        }
        .photosPicker(
            isPresented: Binding(get: { store.photosPresented }, set: { store.send(.showPhotos($0)) }),
            selection: Binding(get: { store.photos }, set: { store.send(.photosSelected($0)) }),
            maxSelectionCount: 10, matching: .any(of: [.images, .screenshots]))
        .fileImporter(isPresented: Binding(get: { store.filesPresented }, set: { store.send(.showFiles($0)) }),
                      allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            switch result {
            case .success(let urls): store.send(.filesSelected(urls))
            case .failure(let error): store.send(.importFailed(error.localizedDescription))
            }
        }
        .fullScreenCover(isPresented: Binding(get: { store.cameraPresented }, set: { store.send(.showCamera($0)) })) {
            CameraPicker { store.send(.cameraCaptured($0)) }.ignoresSafeArea()
        }
        .alert("Couldn’t Attach", isPresented: Binding(get: { store.importError != nil }, set: { if !$0 { store.send(.dismissError) } })) {
            Button("OK", role: .cancel) { store.send(.dismissError) }
        } message: {
            Text(store.importError ?? "")
        }
    }

    private func menu<MenuLabel: View>(@ViewBuilder label: () -> MenuLabel) -> some View {
        Menu {
            Button("Photos", systemImage: "photo.on.rectangle") { store.send(.showPhotos(true)) }
            if UIImagePickerController.isSourceTypeAvailable(.camera) {
                Button("Camera", systemImage: "camera") { store.send(.showCamera(true)) }
            }
            Button("Files", systemImage: "folder") { store.send(.showFiles(true)) }
        } label: {
            label()
        }
    }
}
#endif
