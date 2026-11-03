import Foundation

/// A daemon newer than the app may send a kind of link, item, field or button the app does not
/// know. Only that element is affected: it is left out, shown without its link, or shown disabled,
/// and the rest of the view loads. A known kind with a broken payload still fails, so a daemon bug
/// is never hidden.
public extension ContractCodec {
    /// A tag this app does not know, thrown only by the tagged decoders.
    struct UnknownValue: Error, Equatable, Sendable {
        public let kind: String
    }
}

/// A value a newer daemon added decodes as `fallback`: a new tone or icon draws plain, never fails the view.
public protocol FallbackDecodable: RawRepresentable<String>, Decodable {
    static var fallback: Self { get }
}

public extension FallbackDecodable {
    init(from decoder: any Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .fallback
    }
}

/// A value whose meaning the app cannot guess: an unknown one throws `UnknownValue` for its container to handle.
public protocol KnownValueDecodable: RawRepresentable<String>, Decodable {}

public extension KnownValueDecodable {
    init(from decoder: any Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        guard let value = Self(rawValue: raw) else { throw ContractCodec.UnknownValue(kind: raw) }
        self = value
    }
}

/// One element of a list, or nothing when its kind is unknown.
struct Known<Value: Decodable>: Decodable {
    let value: Value?

    init(from decoder: any Decoder) throws {
        do { value = try Value(from: decoder) } catch is ContractCodec.UnknownValue { value = nil }
    }
}

extension KeyedDecodingContainer {
    func decodeKnownIfPresent<Value: Decodable>(_ type: Value.Type, forKey key: Key) throws -> Value? {
        do { return try decodeIfPresent(type, forKey: key) } catch is ContractCodec.UnknownValue { return nil }
    }

    func decodeKnownElementsIfPresent<Value: Decodable>(_ type: [Value].Type, forKey key: Key) throws -> [Value] {
        try decodeIfPresent([Known<Value>].self, forKey: key)?.compactMap(\.value) ?? []
    }

    func decodeOfferedActions(forKey key: Key) throws -> [Action] {
        try decodeIfPresent([Action].self, forKey: key)?.filter { $0.link?.isOffered ?? true } ?? []
    }
}
