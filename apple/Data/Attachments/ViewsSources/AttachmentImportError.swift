import Foundation

public enum AttachmentImportError: LocalizedError {
    case invalidSize, unavailable
    public var errorDescription: String? {
        switch self {
        case .invalidSize: String(localized: "Choose a non-empty file no larger than 20 MiB.")
        case .unavailable: String(localized: "The attachment could not be loaded.")
        }
    }
}
