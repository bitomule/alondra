import Foundation

public enum AttachmentImportError: LocalizedError {
    case invalidSize, unavailable
    public var errorDescription: String? {
        switch self {
        case .invalidSize: "Choose a non-empty file no larger than 20 MiB."
        case .unavailable: "The attachment could not be loaded."
        }
    }
}
