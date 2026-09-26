import AlondraShared
import AlondraAttachmentsFeature
import ComposableArchitecture
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// The original attachment menu, with presentation and import owned by TCA.
struct ServiceAttachButton: View {
    let store: StoreOf<AttachmentFeature>

    var body: some View {
        Menu {
            Button("Photos", systemImage: "photo.on.rectangle") { store.send(.showPhotos(true)) }
            if UIImagePickerController.isSourceTypeAvailable(.camera) {
                Button("Camera", systemImage: "camera") { store.send(.showCamera(true)) }
            }
            Button("Files", systemImage: "folder") { store.send(.showFiles(true)) }
        } label: {
            Image(systemName: "paperclip")
                .font(.body.weight(.semibold))
                .foregroundStyle(.secondary)
                .frame(width: 36, height: 36)
        }
        .accessibilityLabel("Attach")
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
}
