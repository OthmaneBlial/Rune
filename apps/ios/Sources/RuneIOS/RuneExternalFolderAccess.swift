import Foundation

public enum RuneFolderAccessError: LocalizedError {
    case invalidFolder
    case invalidBookmarkName
    case bookmarkMissing(String)
    case bookmarkAlreadyExists(String)
    case bookmarkResolutionFailed(String)
    case bookmarkStorageLimit
    case folderUnavailable(String)

    public var errorDescription: String? {
        switch self {
        case .invalidFolder:
            return "Rune can only open a readable directory."
        case .invalidBookmarkName:
            return "The folder bookmark name is invalid."
        case .bookmarkMissing(let name):
            return "Rune has no saved folder bookmark named \(name)."
        case .bookmarkAlreadyExists(let name):
            return "Rune already has a saved folder bookmark named \(name)."
        case .bookmarkResolutionFailed(let name):
            return "Rune could not resolve the saved folder bookmark \(name)."
        case .bookmarkStorageLimit:
            return "Rune cannot store more folder bookmark data."
        case .folderUnavailable(let path):
            return "Rune cannot access the selected folder at \(path)."
        }
    }
}

/// Keeps a security-scoped directory access assertion alive for one Rust
/// session. The caller must retain this object for as long as the session uses
/// the external folder.
public final class RuneScopedFolder {
    public let url: URL
    private var startedAccessing = false

    fileprivate init(url: URL) throws {
        self.url = url.standardizedFileURL
        guard self.url.isFileURL else {
            throw RuneFolderAccessError.invalidFolder
        }
        startedAccessing = self.url.startAccessingSecurityScopedResource()
        var isDirectory = ObjCBool(false)
        guard FileManager.default.fileExists(atPath: self.url.path, isDirectory: &isDirectory),
              isDirectory.boolValue else {
            if startedAccessing {
                self.url.stopAccessingSecurityScopedResource()
                startedAccessing = false
            }
            throw RuneFolderAccessError.invalidFolder
        }
        guard FileManager.default.isReadableFile(atPath: self.url.path) else {
            if startedAccessing {
                self.url.stopAccessingSecurityScopedResource()
                startedAccessing = false
            }
            throw RuneFolderAccessError.folderUnavailable(self.url.path)
        }
    }

    public func stopAccessing() {
        guard startedAccessing else { return }
        url.stopAccessingSecurityScopedResource()
        startedAccessing = false
    }

    deinit {
        stopAccessing()
    }
}

/// Stores and resolves user-approved external directory bookmarks. The
/// bookmark bytes remain Apple-side; Rust receives only the currently approved
/// filesystem root through its existing VFS session constructor.
public final class RuneExternalFolderAccess {
    public static let shared = RuneExternalFolderAccess()

    private static let storageKey = "rune.external-folder-bookmarks.v1"
    private static let lastActiveKey = "rune.external-folder-last-active.v1"
    private static let maximumBookmarks = 128
    private static let maximumNameCharacters = 64
    private static let maximumStorageBytes = 512 * 1024

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public var names: [String] {
        bookmarks.keys.sorted()
    }

    public var lastActiveName: String? {
        defaults.string(forKey: Self.lastActiveKey)
    }

    public func save(url: URL, as name: String) throws {
        guard isValidBookmarkName(name) else {
            throw RuneFolderAccessError.invalidBookmarkName
        }
        let folder = try RuneScopedFolder(url: url)
        defer { folder.stopAccessing() }
        let bookmark = try folder.url.bookmarkData(
            options: [.withSecurityScope],
            includingResourceValuesForKeys: nil,
            relativeTo: nil
        )
        var next = bookmarks
        next[name] = bookmark
        guard next.count <= Self.maximumBookmarks,
              next.reduce(0, { $0 + $1.key.utf8.count + $1.value.count }) <= Self.maximumStorageBytes else {
            throw RuneFolderAccessError.bookmarkStorageLimit
        }
        defaults.set(next, forKey: Self.storageKey)
        defaults.set(name, forKey: Self.lastActiveKey)
    }

    public func open(named name: String) throws -> RuneScopedFolder {
        guard let bookmark = bookmarks[name] else {
            throw RuneFolderAccessError.bookmarkMissing(name)
        }
        var isStale = false
        let url: URL
        do {
            url = try URL(
                resolvingBookmarkData: bookmark,
                options: [.withSecurityScope],
                relativeTo: nil,
                bookmarkDataIsStale: &isStale
            )
        } catch {
            throw RuneFolderAccessError.bookmarkResolutionFailed(name)
        }
        let folder = try RuneScopedFolder(url: url)
        if isStale {
            try save(url: folder.url, as: name)
        }
        defaults.set(name, forKey: Self.lastActiveKey)
        return folder
    }

    public func open(url: URL) throws -> RuneScopedFolder {
        try RuneScopedFolder(url: url)
    }

    public func remove(named name: String) {
        var next = bookmarks
        next.removeValue(forKey: name)
        defaults.set(next, forKey: Self.storageKey)
        if lastActiveName == name {
            defaults.removeObject(forKey: Self.lastActiveKey)
        }
    }

    public func rename(named oldName: String, to newName: String) throws {
        guard isValidBookmarkName(oldName), isValidBookmarkName(newName) else {
            throw RuneFolderAccessError.invalidBookmarkName
        }
        guard oldName != newName else { return }
        guard let bookmark = bookmarks[oldName] else {
            throw RuneFolderAccessError.bookmarkMissing(oldName)
        }
        var next = bookmarks
        next.removeValue(forKey: oldName)
        guard next[newName] == nil else {
            throw RuneFolderAccessError.bookmarkAlreadyExists(newName)
        }
        next[newName] = bookmark
        guard next.reduce(0, { $0 + $1.key.utf8.count + $1.value.count }) <= Self.maximumStorageBytes else {
            throw RuneFolderAccessError.bookmarkStorageLimit
        }
        defaults.set(next, forKey: Self.storageKey)
        if lastActiveName == oldName {
            defaults.set(newName, forKey: Self.lastActiveKey)
        }
    }

    public func suggestedName(for url: URL) -> String {
        let raw = url.lastPathComponent.isEmpty ? "folder" : url.lastPathComponent
        let scalarLimited = String(raw.prefix(Self.maximumNameCharacters))
        let cleaned = scalarLimited.map { character in
            character.isLetter || character.isNumber || character == "_" || character == "-" ? character : "-"
        }
        let name = String(cleaned).trimmingCharacters(in: CharacterSet(charactersIn: "-"))
        return name.isEmpty ? "folder" : name
    }

    private var bookmarks: [String: Data] {
        defaults.dictionary(forKey: Self.storageKey) as? [String: Data] ?? [:]
    }

    private func isValidBookmarkName(_ name: String) -> Bool {
        !name.isEmpty
            && name.count <= Self.maximumNameCharacters
            && name.allSatisfy {
                $0.isLetter || $0.isNumber || $0 == "_" || $0 == "-" || $0 == "."
            }
    }
}
