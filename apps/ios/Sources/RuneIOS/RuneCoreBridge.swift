import Foundation

private struct RuneFFIOutput {
    let stdout: UnsafeMutablePointer<CChar>?
    let stderr: UnsafeMutablePointer<CChar>?
    let status: Int32
}

@_silgen_name("rune_session_new")
private func rune_session_new(_ root: UnsafePointer<CChar>) -> OpaquePointer?

@_silgen_name("rune_session_destroy")
private func rune_session_destroy(_ handle: OpaquePointer?)

@_silgen_name("rune_session_execute")
private func rune_session_execute(
    _ handle: OpaquePointer?,
    _ input: UnsafePointer<CChar>
) -> RuneFFIOutput

@_silgen_name("rune_session_current_directory")
private func rune_session_current_directory(_ handle: OpaquePointer?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("rune_session_history")
private func rune_session_history(_ handle: OpaquePointer?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("rune_session_commands")
private func rune_session_commands(_ handle: OpaquePointer?) -> UnsafeMutablePointer<CChar>?

@_silgen_name("rune_string_free")
private func rune_string_free(_ value: UnsafeMutablePointer<CChar>?)

public struct RuneCommandResult: Equatable, Sendable {
    public let stdout: String
    public let stderr: String
    public let status: Int32

    public init(stdout: String, stderr: String, status: Int32) {
        self.stdout = stdout
        self.stderr = stderr
        self.status = status
    }
}

public enum RuneBridgeError: LocalizedError {
    case sessionInitializationFailed(URL)

    public var errorDescription: String? {
        switch self {
        case .sessionInitializationFailed(let root):
            return "Rune could not open its sandbox root at \(root.path)."
        }
    }
}

public final class RuneFFISession {
    private var handle: OpaquePointer?

    public init(rootURL: URL) throws {
        let created = rootURL.path.withCString { rune_session_new($0) }
        guard let created else {
            throw RuneBridgeError.sessionInitializationFailed(rootURL)
        }
        handle = created
    }

    deinit {
        rune_session_destroy(handle)
    }

    public var currentDirectory: String {
        guard let pointer = rune_session_current_directory(handle) else {
            return "~"
        }
        defer { rune_string_free(pointer) }
        return String(cString: pointer)
    }

    public func execute(_ command: String) -> RuneCommandResult {
        let raw = command.withCString { rune_session_execute(handle, $0) }
        defer {
            rune_string_free(raw.stdout)
            rune_string_free(raw.stderr)
        }
        let stdout = raw.stdout.map { String(cString: $0) } ?? ""
        let stderr = raw.stderr.map { String(cString: $0) } ?? ""
        return RuneCommandResult(stdout: stdout, stderr: stderr, status: raw.status)
    }

    public func history() -> [String] {
        guard let pointer = rune_session_history(handle) else {
            return []
        }
        defer { rune_string_free(pointer) }
        let value = String(cString: pointer)
        guard !value.isEmpty else { return [] }
        return value.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
    }

    public func commands() -> [String] {
        guard let pointer = rune_session_commands(handle) else {
            return []
        }
        defer { rune_string_free(pointer) }
        let value = String(cString: pointer)
        guard !value.isEmpty else { return [] }
        return value.split(separator: "\n").map(String.init)
    }
}
