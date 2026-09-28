import CryptoKit
import Foundation

public enum AttachmentTransferError: Error, Equatable, Sendable {
    case invalidSize, invalidName, invalidResponse
}
